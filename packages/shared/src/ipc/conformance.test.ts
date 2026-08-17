/**
 * The check that fails when the TypeScript and the Rust disagree.
 *
 * `protocol.ts` is written by hand. That is only defensible if something fails
 * the moment it stops describing the protocol `crates/ipc` implements, and this
 * is that something. It reads `protocol-schema.json`, which
 * `crates/ipc/src/schema.rs` derives from the Rust types — field names and
 * optionality from `serde`, wire strings from serialising real values, and the
 * verdict on every sample frame from running it through the real deserialiser —
 * and holds the types in this directory against it.
 *
 * Three kinds of disagreement fail here:
 *
 * - **A value one side knows and the other does not.** Every enumeration is
 *   compared both ways, and each list is tied to its type by construction, so a
 *   list that satisfies this file is a type that does too.
 * - **A field one side has and the other does not.** {@link FieldSpec} is a
 *   mapped type over the interface itself: a field added to the TypeScript and
 *   not to the descriptor does not compile, and a field added to the Rust and
 *   not to the TypeScript fails here.
 * - **A frame the two sides read differently.** Every sample is parsed and its
 *   discriminant compared with the one the recorder's own deserialiser
 *   produced, including the frames from a build newer than this one.
 *
 * The Rust half of the same check is
 * `the_committed_schema_is_the_one_this_build_produces` in
 * `crates/ipc/src/schema.rs`, which insists the committed schema is still what
 * the Rust types say. Neither half is enough alone: this one would happily
 * agree with a schema nobody had regenerated.
 */

import { describe, expect, it } from 'vitest';

import { LENGTH_PREFIX_BYTES, MAX_FRAME_BYTES } from './frame';
import { parseClientMessage, parseServerMessage } from './parse';
import type {
  ActiveRecording,
  AddBookmarkParams,
  ApplySettingsParams,
  AudioDevice,
  AudioDevices,
  AudioDevicesReply,
  SettingEntry,
  SettingsReply,
  SettingsView,
  BookmarkAddedReply,
  ScreenshotSummary,
  ScreenshotTakenReply,
  TakeScreenshotParams,
  BookmarkSummary,
  ClientMessage,
  CommandName,
  ConnectionRole,
  ErrorCode,
  ErrorDetail,
  ErrorOutcome,
  EventStream,
  ExportRecordingParams,
  ExportSummary,
  ConflictingHotkey,
  Feature,
  Hello,
  HotkeyBinding,
  HotkeysReply,
  HotkeyStateName,
  IdleStatus,
  KnownCommandName,
  KnownErrorDetailName,
  KnownEventName,
  LibraryClip,
  LibraryGame,
  LibraryEventsReply,
  LibraryTrashReply,
  RestoredReply,
  TrashEmptiedReply,
  FavouritedReply,
  PluginsReply,
  LibraryGamesReply,
  LibraryRecording,
  LibrarySession,
  LibrarySessionPage,
  LibrarySessionsParams,
  LibrarySessionsReply,
  NotImplementedDetail,
  OkOutcome,
  PeerIdentity,
  PongReply,
  ProtocolError,
  RecorderEvent,
  RecorderRequest,
  RecorderResponse,
  RecorderState,
  RecorderStatus,
  RecordingExportedReply,
  RecordingFailedEvent,
  RecordingStartedReply,
  RecordingStatus,
  RecordingStoppedReply,
  RecordingSummary,
  RegisteredHotkey,
  Reply,
  ReplyName,
  ReplaySavedReply,
  ReplaySummary,
  SaveReplayParams,
  ServerMessage,
  SessionEndedEvent,
  SessionRecording,
  SessionSummary,
  StartRecordingParams,
  StatusChangedEvent,
  ShutdownParams,
  ShuttingDownReply,
  StatusReply,
  StopRecordingParams,
  UnboundHotkey,
  UnsupportedProtocolVersionDetail,
  WatchingStatus,
  Welcome,
} from './protocol';
import {
  CLIENT_MESSAGE_TYPES,
  COMMANDS,
  CONNECTION_ROLES,
  DEFAULT_CONNECTION_ROLE,
  END_REASONS,
  ERROR_CODES,
  ERROR_DETAILS,
  EVENTS,
  EVENT_STREAMS,
  FEATURES,
  HOTKEY_STATES,
  MAX_CONCURRENT_CONNECTIONS,
  OUTCOMES,
  PROTOCOL_VERSION,
  RECORDER_STATES,
  REPLIES,
  SERVER_MESSAGE_TYPES,
  SUPPORTED_PROTOCOL_VERSIONS,
  isRecognisedErrorDetail,
} from './protocol';
import rawSchema from './protocol-schema.json';

/** The schema document, as this build expects to find it. */
interface SchemaDocument {
  readonly schema_format: number;
  readonly protocol_version: number;
  readonly supported_protocol_versions: readonly number[];
  readonly framing: {
    readonly length_prefix_bytes: number;
    readonly length_prefix_endianness: string;
    readonly max_frame_bytes: number;
  };
  readonly max_concurrent_connections: number;
  readonly envelopes: Readonly<Record<string, Readonly<Record<string, string>>>>;
  readonly enumerations: Readonly<
    Record<string, { readonly values: readonly string[]; readonly tolerates_unrecognised: boolean }>
  >;
  readonly structures: Readonly<
    Record<string, { readonly required: readonly string[]; readonly optional: readonly string[] }>
  >;
  readonly commands: readonly {
    readonly name: string;
    readonly params: string | null;
    readonly reply: string | null;
    readonly available_in_this_build: boolean;
  }[];
  readonly samples: readonly {
    readonly name: string;
    readonly direction: string;
    readonly frame: unknown;
    readonly parses: boolean;
    readonly discriminant: string | null;
  }[];
}

const schema = rawSchema as unknown as SchemaDocument;

/** The schema shape this file knows how to read. */
const SCHEMA_FORMAT = 1;

/** Whether a type admits values outside the ones it lists. */
type IsExtensible<T extends string> = string extends T ? true : false;

/** Whether a tagged union has a member for a tag it does not recognise. */
type ToleratesUnrecognised<Tag> = undefined extends Tag ? true : false;

/** Two types are the same type. */
type AssertEqual<A, B> = [A] extends [B] ? ([B] extends [A] ? true : false) : false;

/**
 * The fields of an interface, and which of them may be left out.
 *
 * The values are not free: `undefined extends T[K]` decides them, so a
 * descriptor calling a required field optional does not compile, and a field
 * added to or removed from the interface breaks the descriptor until it is
 * updated. That is what makes the comparison with the Rust below meaningful
 * rather than a comparison of two hand-written lists.
 */
type FieldSpec<T> = {
  readonly [K in keyof Required<T>]-?: undefined extends T[K] ? 'optional' : 'required';
};

interface Structure {
  readonly required: readonly string[];
  readonly optional: readonly string[];
}

/** The Rust-facing shape of one TypeScript interface. */
function fields<T>(spec: FieldSpec<T>): Structure {
  const required: string[] = [];
  const optional: string[] = [];
  for (const [name, kind] of Object.entries(spec as unknown as Record<string, string>)) {
    (kind === 'optional' ? optional : required).push(name);
  }
  return { required: required.sort(), optional: optional.sort() };
}

interface EnumerationMirror {
  readonly values: readonly string[];
  readonly toleratesUnrecognised: boolean;
}

/** An open or closed set of wire strings, with its tolerance tied to its type. */
function enumeration<T extends string>(
  values: readonly string[],
  toleratesUnrecognised: IsExtensible<T>,
): EnumerationMirror {
  return { values, toleratesUnrecognised };
}

/** The same, for a union that expresses its tolerance as a variant. */
function taggedEnumeration<Tag>(
  values: readonly string[],
  toleratesUnrecognised: ToleratesUnrecognised<Tag>,
): EnumerationMirror {
  return { values, toleratesUnrecognised };
}

const TYPESCRIPT_ENUMERATIONS: Readonly<Record<string, EnumerationMirror>> = {
  client_message_type: enumeration<ClientMessage['type']>(CLIENT_MESSAGE_TYPES, false),
  server_message_type: enumeration<ServerMessage['type']>(SERVER_MESSAGE_TYPES, false),
  connection_role: enumeration<ConnectionRole>(CONNECTION_ROLES, true),
  event_stream: enumeration<EventStream>(EVENT_STREAMS, true),
  feature: enumeration<Feature>(FEATURES, true),
  command: enumeration<CommandName>(COMMANDS, true),
  error_code: enumeration<ErrorCode>(ERROR_CODES, true),
  error_detail: taggedEnumeration<ErrorDetail['detail']>(ERROR_DETAILS, true),
  end_reason: enumeration<RecordingSummary['end_reason']>(END_REASONS, true),
  event: taggedEnumeration<RecorderEvent['event']>(EVENTS, true),
  outcome: enumeration<'ok' | 'error'>(OUTCOMES, false),
  reply: enumeration<ReplyName>(REPLIES, false),
  recorder_state: enumeration<RecorderState>(RECORDER_STATES, false),
  hotkey_state: enumeration<HotkeyStateName>(HOTKEY_STATES, false),
};

const TYPESCRIPT_STRUCTURES: Readonly<Record<string, Structure>> = {
  peer_identity: fields<PeerIdentity>({ name: 'required', version: 'required' }),
  hello: fields<Hello>({
    protocol_version: 'required',
    client: 'required',
    role: 'optional',
    streams: 'optional',
  }),
  welcome: fields<Welcome>({
    protocol_version: 'required',
    recorder: 'required',
    role: 'required',
    features: 'required',
    streams: 'optional',
  }),
  request: fields<RecorderRequest>({ id: 'required', command: 'required', params: 'optional' }),
  response: fields<RecorderResponse>({ id: 'required', outcome: 'required' }),
  protocol_error: fields<ProtocolError>({
    code: 'required',
    message: 'required',
    detail: 'optional',
  }),
  start_recording: fields<StartRecordingParams>({
    window: 'optional',
    process: 'optional',
    pid: 'optional',
    output: 'optional',
    overwrite: 'optional',
    resolution: 'optional',
    framerate: 'optional',
    codec: 'optional',
    encoder: 'optional',
    microphone: 'optional',
    system_audio: 'optional',
    replay_seconds: 'optional',
  }),
  stop_recording: fields<StopRecordingParams>({ recording_id: 'optional' }),
  add_bookmark: fields<AddBookmarkParams>({
    recording_id: 'optional',
    label: 'optional',
    colour: 'optional',
    duration_seconds: 'optional',
    lead_seconds: 'optional',
  }),
  take_screenshot: fields<TakeScreenshotParams>({
    recording_id: 'optional',
    window: 'optional',
    process: 'optional',
    pid: 'optional',
    format: 'optional',
  }),
  shutdown: fields<ShutdownParams>({ finalise_recording: 'optional' }),
  active_recording: fields<ActiveRecording>({
    recording_id: 'required',
    output: 'required',
    target: 'required',
    elapsed_ms: 'required',
    replay_seconds: 'optional',
    session: 'optional',
  }),
  session_summary: fields<SessionSummary>({
    session_id: 'required',
    game_id: 'optional',
    game_name: 'optional',
    started_at: 'required',
    ended_at: 'optional',
    end_reason: 'optional',
    recordings: 'required',
  }),
  session_recording: fields<SessionRecording>({
    session_index: 'required',
    output: 'required',
    outcome: 'optional',
    duration_ms: 'optional',
  }),
  recording_summary: fields<RecordingSummary>({
    output: 'required',
    duration_ms: 'required',
    end_reason: 'required',
    frames_encoded: 'required',
    frames_skipped_for_rate: 'required',
    frames_dropped_writer_behind: 'required',
    sustained_framerate: 'optional',
    encoder: 'required',
    codec: 'required',
    width: 'required',
    height: 'required',
  }),
  bookmark_summary: fields<BookmarkSummary>({
    recording_id: 'required',
    at_seconds: 'required',
    pressed_at_seconds: 'required',
    lead_seconds: 'required',
    label: 'optional',
    colour: 'optional',
    duration_seconds: 'optional',
    bookmarks_file: 'required',
    bookmarks_in_recording: 'required',
  }),
  save_replay: fields<SaveReplayParams>({
    recording_id: 'optional',
    duration_seconds: 'optional',
    output: 'optional',
  }),
  replay_summary: fields<ReplaySummary>({
    path: 'required',
    recording_id: 'required',
    requested_seconds: 'required',
    duration_seconds: 'required',
    source_start_seconds: 'required',
    source_end_seconds: 'required',
    leading_slack_seconds: 'required',
    complete: 'required',
    shortfall_seconds: 'required',
    bytes: 'required',
  }),
  screenshot_summary: fields<ScreenshotSummary>({
    path: 'required',
    format: 'required',
    width: 'required',
    height: 'required',
    bytes: 'required',
    recording_id: 'optional',
    at_seconds: 'optional',
  }),
  library_sessions: fields<LibrarySessionsParams>({
    limit: 'optional',
    after: 'optional',
    query: 'optional',
  }),
  library_session_page: fields<LibrarySessionPage>({
    sessions: 'required',
    next_cursor: 'optional',
  }),
  library_session: fields<LibrarySession>({
    session_id: 'required',
    game_id: 'optional',
    game_name: 'optional',
    started_at: 'required',
    ended_at: 'optional',
    end_reason: 'optional',
    favourite: 'required',
    recordings: 'required',
    clips: 'required',
  }),
  library_recording: fields<LibraryRecording>({
    recording_id: 'required',
    session_index: 'required',
    path: 'required',
    started_at: 'required',
    ended_at: 'optional',
    outcome: 'optional',
    end_reason: 'optional',
    duration_seconds: 'optional',
    width: 'optional',
    height: 'optional',
    size_bytes: 'optional',
    missing_since: 'optional',
    favourite: 'required',
    tags: 'required',
  }),
  library_clip: fields<LibraryClip>({
    clip_id: 'required',
    path: 'required',
    title: 'optional',
    created_at: 'required',
    duration_seconds: 'optional',
    size_bytes: 'optional',
    missing_since: 'optional',
    favourite: 'required',
    tags: 'required',
  }),
  library_game: fields<LibraryGame>({
    game_id: 'optional',
    name: 'optional',
    first_seen_at: 'optional',
    last_played_at: 'optional',
    sessions: 'required',
    recordings: 'required',
    clips: 'required',
    favourites: 'required',
    bytes: 'required',
    missing: 'required',
  }),
  export_recording: fields<ExportRecordingParams>({
    source: 'required',
    destination: 'required',
  }),
  export_summary: fields<ExportSummary>({
    source: 'required',
    destination: 'required',
    duration_ms: 'required',
    packets: 'required',
    bytes: 'required',
    elapsed_ms: 'required',
    lossless: 'required',
    losses: 'optional',
  }),
  'outcome.ok': fields<OkOutcome>({ ok: 'required' }),
  'outcome.error': fields<ErrorOutcome>({ error: 'required' }),
  'reply.pong': fields<PongReply>({ reply: 'required' }),
  'reply.status': fields<StatusReply>({ reply: 'required', status: 'required' }),
  'reply.recording_started': fields<RecordingStartedReply>({
    reply: 'required',
    recording_id: 'required',
    output: 'required',
  }),
  'reply.recording_stopped': fields<RecordingStoppedReply>({
    reply: 'required',
    summary: 'required',
  }),
  'reply.bookmark_added': fields<BookmarkAddedReply>({
    reply: 'required',
    bookmark: 'required',
  }),
  'reply.screenshot_taken': fields<ScreenshotTakenReply>({
    reply: 'required',
    screenshot: 'required',
  }),
  'reply.replay_saved': fields<ReplaySavedReply>({
    reply: 'required',
    clip: 'required',
  }),
  'reply.library_sessions': fields<LibrarySessionsReply>({
    reply: 'required',
    page: 'required',
  }),
  'reply.library_games': fields<LibraryGamesReply>({
    reply: 'required',
    games: 'required',
  }),
  'reply.library_events': fields<LibraryEventsReply>({
    reply: 'required',
    lane: 'required',
  }),
  'reply.library_trash': fields<LibraryTrashReply>({
    reply: 'required',
    trash: 'required',
  }),
  'reply.restored': fields<RestoredReply>({
    reply: 'required',
    restored: 'required',
  }),
  'reply.trash_emptied': fields<TrashEmptiedReply>({
    reply: 'required',
    emptied: 'required',
  }),
  'reply.favourited': fields<FavouritedReply>({
    reply: 'required',
    mark: 'required',
  }),
  'reply.plugins': fields<PluginsReply>({
    reply: 'required',
    installed: 'required',
    refused: 'required',
  }),
  'reply.recording_exported': fields<RecordingExportedReply>({
    reply: 'required',
    export: 'required',
  }),
  'reply.shutting_down': fields<ShuttingDownReply>({
    reply: 'required',
    finalising: 'optional',
  }),
  hotkey_binding: fields<HotkeyBinding>({
    action: 'required',
    label: 'required',
    hotkey: 'optional',
    state: 'required',
    handled: 'required',
    unavailable: 'optional',
  }),
  'hotkey_state.unbound': fields<UnboundHotkey>({ state: 'required' }),
  'hotkey_state.registered': fields<RegisteredHotkey>({ state: 'required' }),
  'hotkey_state.conflict': fields<ConflictingHotkey>({
    state: 'required',
    reason: 'required',
  }),
  'reply.hotkeys': fields<HotkeysReply>({ reply: 'required', hotkeys: 'required' }),
  setting_entry: fields<SettingEntry>({
    key: 'required',
    label: 'required',
    value: 'required',
    overridden: 'required',
    choices: 'optional',
    accepted: 'required',
    applies: 'required',
    unavailable: 'optional',
  }),
  settings_view: fields<SettingsView>({ file: 'required', settings: 'required' }),
  'reply.settings': fields<SettingsReply>({ reply: 'required', settings: 'required' }),
  audio_device: fields<AudioDevice>({ name: 'required', is_default: 'required' }),
  audio_devices: fields<AudioDevices>({ microphones: 'required' }),
  'reply.audio_devices': fields<AudioDevicesReply>({ reply: 'required', devices: 'required' }),
  apply_settings: fields<ApplySettingsParams>({ values: 'optional' }),
  'recorder_status.idle': fields<IdleStatus>({ state: 'required' }),
  'recorder_status.watching': fields<WatchingStatus>({ state: 'required', session: 'optional' }),
  'recorder_status.recording': fields<RecordingStatus>({
    state: 'required',
    recording_id: 'required',
    output: 'required',
    target: 'required',
    elapsed_ms: 'required',
    replay_seconds: 'optional',
    session: 'optional',
  }),
  'event.status_changed': fields<StatusChangedEvent>({ event: 'required', status: 'required' }),
  'event.session_ended': fields<SessionEndedEvent>({ event: 'required', session: 'required' }),
  'event.recording_failed': fields<RecordingFailedEvent>({
    event: 'required',
    recording_id: 'required',
    error: 'required',
  }),
  'error_detail.unsupported_protocol_version': fields<UnsupportedProtocolVersionDetail>({
    detail: 'required',
    requested: 'required',
    supported: 'required',
    recorder_version: 'required',
  }),
  'error_detail.not_implemented': fields<NotImplementedDetail>({
    detail: 'required',
    subsystem: 'required',
    milestone: 'required',
    tracking_issue: 'required',
  }),
};

/**
 * What each command takes and gives back, in the names of the structures above.
 *
 * `params` and `reply` are keys of {@link TYPESCRIPT_STRUCTURES}, so a command
 * cannot claim a shape this file does not describe, and the structures those
 * names point at are themselves held against the Rust. `available_in_this_build`
 * is the recorder's own answer to whether a command is performed or refused with
 * `not_implemented`: a build that gains one changes the schema and this list has
 * to follow, which is the point at which somebody asks whether the interface
 * should now be offering it.
 */
const TYPESCRIPT_COMMANDS: readonly {
  readonly name: KnownCommandName;
  readonly params: keyof typeof TYPESCRIPT_STRUCTURES | null;
  readonly reply: keyof typeof TYPESCRIPT_STRUCTURES | null;
  readonly available_in_this_build: boolean;
}[] = [
  { name: 'ping', params: null, reply: 'reply.pong', available_in_this_build: true },
  { name: 'get_status', params: null, reply: 'reply.status', available_in_this_build: true },
  {
    name: 'start_recording',
    params: 'start_recording',
    reply: 'reply.recording_started',
    available_in_this_build: true,
  },
  {
    name: 'stop_recording',
    params: 'stop_recording',
    reply: 'reply.recording_stopped',
    available_in_this_build: true,
  },
  {
    name: 'add_bookmark',
    params: 'add_bookmark',
    reply: 'reply.bookmark_added',
    available_in_this_build: true,
  },
  {
    name: 'take_screenshot',
    params: 'take_screenshot',
    reply: 'reply.screenshot_taken',
    available_in_this_build: true,
  },
  {
    name: 'save_replay',
    params: 'save_replay',
    reply: 'reply.replay_saved',
    available_in_this_build: true,
  },
  {
    name: 'library_sessions',
    params: 'library_sessions',
    reply: 'reply.library_sessions',
    available_in_this_build: true,
  },
  {
    name: 'library_games',
    params: null,
    reply: 'reply.library_games',
    available_in_this_build: true,
  },
  {
    name: 'library_events',
    params: 'library_events',
    reply: 'reply.library_events',
    available_in_this_build: true,
  },
  {
    name: 'library_trash',
    params: 'library_trash',
    reply: 'reply.library_trash',
    available_in_this_build: true,
  },
  {
    name: 'restore_from_trash',
    params: 'restore_from_trash',
    reply: 'reply.restored',
    available_in_this_build: true,
  },
  {
    name: 'empty_trash',
    params: 'empty_trash',
    reply: 'reply.trash_emptied',
    available_in_this_build: true,
  },
  {
    name: 'set_favourite',
    params: 'set_favourite',
    reply: 'reply.favourited',
    available_in_this_build: true,
  },
  {
    name: 'plugins',
    params: null,
    reply: 'reply.plugins',
    available_in_this_build: true,
  },
  {
    name: 'export_recording',
    params: 'export_recording',
    reply: 'reply.recording_exported',
    available_in_this_build: true,
  },
  {
    name: 'get_hotkeys',
    params: null,
    reply: 'reply.hotkeys',
    available_in_this_build: true,
  },
  {
    name: 'get_settings',
    params: null,
    reply: 'reply.settings',
    available_in_this_build: true,
  },
  {
    // The command that used to be the one nobody performed. Both settings
    // commands answer with the same reply, because what a change produced is
    // the settings as they now stand (issue #51).
    name: 'apply_settings',
    params: 'apply_settings',
    reply: 'reply.settings',
    available_in_this_build: true,
  },
  {
    name: 'get_audio_devices',
    params: null,
    reply: 'reply.audio_devices',
    available_in_this_build: true,
  },
  {
    name: 'shutdown',
    params: 'shutdown',
    reply: 'reply.shutting_down',
    available_in_this_build: true,
  },
];

/** Which structure each envelope's payload takes, as the types here compose it. */
const TYPESCRIPT_ENVELOPES: Readonly<Record<string, Readonly<Record<string, string>>>> = {
  client: { hello: 'hello', request: 'request' },
  server: { welcome: 'welcome', refused: 'protocol_error', response: 'response', event: 'event' },
};

function errorDiscriminant(error: ProtocolError): string {
  const detail = error.detail;
  if (detail === undefined) {
    return error.code;
  }
  return `${error.code}.${detail.detail ?? 'unrecognised'}`;
}

function replyDiscriminant(reply: Reply): string {
  switch (reply.reply) {
    case 'pong':
      return 'pong';
    case 'status':
      return `status.${reply.status.state}`;
    case 'recording_started':
      return 'recording_started';
    case 'recording_stopped':
      return `recording_stopped.${reply.summary.end_reason}`;
    case 'bookmark_added':
      return 'bookmark_added';
    case 'screenshot_taken':
      return 'screenshot_taken';
    case 'replay_saved':
      return 'replay_saved';
    case 'library_sessions':
      // Whether the page ends the library is part of the path, for the reason
      // `shutting_down`'s is: a mirror that dropped the cursor would otherwise
      // reach the same discriminant for a page that continues and one that does
      // not, and paging is the whole of what this reply is for.
      return reply.page.next_cursor === undefined ? 'library_sessions' : 'library_sessions.more';
    case 'library_games':
      return 'library_games';
    case 'plugins':
      return 'plugins';
    case 'library_events':
      // One discriminant, unlike `library_sessions`: an empty lane is not a
      // different shape, it is the same shape carrying nothing. What tells
      // "none" from "not asked" is that `marks` is always present.
      return 'library_events';
    case 'restored':
      return 'restored';
    case 'trash_emptied':
      // One discriminant: a refusal list that is empty is the same shape
      // carrying nothing, and it is always present.
      return 'trash_emptied';
    case 'favourited':
      // One discriminant: whether the mark changed is a field, not a shape, and
      // a session and a recording differ only in which half of the target is
      // filled in.
      return 'favourited';
    case 'library_trash':
      // One discriminant, for the same reason: an empty trash is the same
      // shape carrying nothing, and there is no paging to lose.
      return 'library_trash';
    case 'hotkeys':
      return 'hotkeys';
    case 'settings':
      // One discriminant: a settings view with nothing in it is the same shape
      // carrying nothing, and `settings` is always present.
      return 'settings';
    case 'audio_devices':
      // The same. A machine with no microphone is an empty list rather than a
      // different reply.
      return 'audio_devices';
    case 'recording_exported':
      // Whether the copy is complete is part of the path, because it is the one
      // thing the window has to say differently: a mirror that dropped
      // `lossless` would reach the same discriminant for an MP4 that holds the
      // whole recording and one that quietly does not.
      return reply.export.lossless ? 'recording_exported' : 'recording_exported.lossy';
    case 'shutting_down':
      // Whether a recording is being finished is the whole of what this reply
      // says, so it is part of the path: dropping the field would otherwise
      // reach the same discriminant either way.
      return reply.finalising === undefined ? 'shutting_down' : 'shutting_down.finalising';
  }
}

function eventDiscriminant(event: RecorderEvent): string {
  switch (event.event) {
    case 'status_changed':
      return `status_changed.${event.status.state}`;
    case 'session_ended':
      // The reason is part of the path, because it is the one thing this event
      // says that a window shows differently — and dropping a reason invented
      // later would otherwise reach the same answer as keeping it.
      return `session_ended.${event.session.end_reason ?? 'unstated'}`;
    case 'recording_failed':
      return 'recording_failed';
    case undefined:
      return 'unrecognised';
  }
}

function clientDiscriminant(message: ClientMessage): string {
  switch (message.type) {
    case 'hello': {
      const role = message.role ?? DEFAULT_CONNECTION_ROLE;
      const streams = message.streams ?? [];
      // The streams are in the path so that a mirror which dropped a stream
      // name it did not recognise reaches a different answer from one that kept
      // it, which is the only thing the "an event stream invented later" sample
      // can prove.
      return streams.length === 0 ? `hello.${role}` : `hello.${role}.${streams.join('+')}`;
    }
    case 'request':
      return `request.${message.command}`;
  }
}

function serverDiscriminant(message: ServerMessage): string {
  switch (message.type) {
    case 'welcome':
      return `welcome.${message.role}`;
    case 'refused':
      return `refused.${errorDiscriminant(message)}`;
    case 'response':
      return 'ok' in message.outcome
        ? `response.ok.${replyDiscriminant(message.outcome.ok)}`
        : `response.error.${errorDiscriminant(message.outcome.error)}`;
    case 'event':
      return `event.${eventDiscriminant(message)}`;
  }
}

function sampleNamed(name: string) {
  const sample = schema.samples.find((candidate) => candidate.name === name);
  if (sample === undefined) {
    throw new Error(`the schema has no \`${name}\` sample`);
  }
  return sample;
}

describe('the schema this check reads', () => {
  it('is a document this build understands', () => {
    expect(schema.schema_format).toBe(SCHEMA_FORMAT);
  });
});

describe('the constants', () => {
  it('agree about the protocol version', () => {
    expect(PROTOCOL_VERSION).toBe(schema.protocol_version);
    expect([...SUPPORTED_PROTOCOL_VERSIONS]).toEqual([...schema.supported_protocol_versions]);
  });

  it('agree about the framing', () => {
    expect(LENGTH_PREFIX_BYTES).toBe(schema.framing.length_prefix_bytes);
    expect(MAX_FRAME_BYTES).toBe(schema.framing.max_frame_bytes);
    expect(schema.framing.length_prefix_endianness).toBe('little');
  });

  it('agree about how many connections a recorder serves', () => {
    expect(MAX_CONCURRENT_CONNECTIONS).toBe(schema.max_concurrent_connections);
  });

  it('agree about the commands, including the ones no build performs yet', () => {
    expect([...COMMANDS]).toEqual(schema.commands.map((command) => command.name));
    expect(TYPESCRIPT_COMMANDS).toEqual(schema.commands);
  });
});

describe('the unions are derived from the lists this check compares', () => {
  it('ties each list to the type built from it', () => {
    // Without these, the comparisons below would be checking constants that
    // nothing forces the types to agree with.
    const replies: AssertEqual<Reply['reply'], ReplyName> = true;
    const states: AssertEqual<RecorderStatus['state'], RecorderState> = true;
    const events: AssertEqual<Exclude<RecorderEvent['event'], undefined>, KnownEventName> = true;
    const details: AssertEqual<
      Exclude<ErrorDetail['detail'], undefined>,
      KnownErrorDetailName
    > = true;
    const client: AssertEqual<ClientMessage['type'], (typeof CLIENT_MESSAGE_TYPES)[number]> = true;
    const server: AssertEqual<ServerMessage['type'], (typeof SERVER_MESSAGE_TYPES)[number]> = true;

    expect([replies, states, events, details, client, server]).toEqual([
      true,
      true,
      true,
      true,
      true,
      true,
    ]);
  });
});

describe('the enumerations', () => {
  it('describe the same set of enumerations', () => {
    expect(Object.keys(TYPESCRIPT_ENUMERATIONS).sort()).toEqual(
      Object.keys(schema.enumerations).sort(),
    );
  });

  for (const [name, rust] of Object.entries(schema.enumerations)) {
    it(`knows every ${name}`, () => {
      const mirror = TYPESCRIPT_ENUMERATIONS[name];
      expect(mirror, `${name} is on the wire and not in protocol.ts`).toBeDefined();
      expect([...(mirror?.values ?? [])].sort()).toEqual([...rust.values].sort());
    });

    it(`treats an unrecognised ${name} the way the recorder does`, () => {
      expect(TYPESCRIPT_ENUMERATIONS[name]?.toleratesUnrecognised).toBe(
        rust.tolerates_unrecognised,
      );
    });
  }
});

describe('the structures', () => {
  it('describe the same set of objects', () => {
    expect(Object.keys(TYPESCRIPT_STRUCTURES).sort()).toEqual(
      Object.keys(schema.structures).sort(),
    );
  });

  for (const [name, rust] of Object.entries(schema.structures)) {
    it(`has the fields of ${name}`, () => {
      const mirror = TYPESCRIPT_STRUCTURES[name];
      expect(mirror, `${name} is on the wire and not in protocol.ts`).toBeDefined();
      expect([...(mirror?.required ?? [])]).toEqual([...rust.required]);
      expect([...(mirror?.optional ?? [])]).toEqual([...rust.optional]);
    });
  }
});

describe('the envelopes', () => {
  it('carry the same payloads', () => {
    expect(TYPESCRIPT_ENVELOPES).toEqual(schema.envelopes);
  });
});

describe('every frame the recorder can send', () => {
  for (const sample of schema.samples) {
    it(`reads ${sample.name} exactly as the recorder does`, () => {
      const result =
        sample.direction === 'client'
          ? parseClientMessage(sample.frame)
          : parseServerMessage(sample.frame);

      expect(
        result.ok,
        result.ok ? '' : `the recorder read this frame and this build did not: ${result.problem}`,
      ).toBe(sample.parses);

      if (result.ok) {
        const discriminant =
          sample.direction === 'client'
            ? clientDiscriminant(result.message as ClientMessage)
            : serverDiscriminant(result.message as ServerMessage);
        expect(discriminant).toBe(sample.discriminant);
      }
    });
  }
});

describe('every reply the recorder can send', () => {
  /**
   * The hole this closes, and it was a real one.
   *
   * `bookmark_added` and `screenshot_taken` were in {@link REPLIES}, in the
   * Rust schema and in {@link TYPESCRIPT_STRUCTURES}, and in no sample frame —
   * so nothing ever ran one through {@link parseServerMessage}, and `parse.ts`
   * could not read either of them. Both lists agreed perfectly about a reply
   * neither side could handle.
   *
   * The lists alone cannot catch that: they compare names. Only a frame proves
   * a reply can be read, so this insists there is one, and the suite above then
   * parses it. `every_reply_the_recorder_can_send_has_a_sample_carrying_it` in
   * `crates/ipc/src/schema.rs` is the same rule from the Rust side, which is
   * what stops the sample being dropped there instead.
   */
  const carried = (reply: ReplyName): boolean =>
    schema.samples.some(
      (sample) =>
        sample.discriminant === `response.ok.${reply}` ||
        (sample.discriminant?.startsWith(`response.ok.${reply}.`) ?? false),
    );

  for (const reply of REPLIES) {
    it(`is proved readable by a sample frame: ${reply}`, () => {
      expect(
        carried(reply),
        `no sample frame carries a \`${reply}\`, so nothing checks this build can read one`,
      ).toBe(true);
    });
  }
});

describe('a recorder newer than this build', () => {
  it('keeps the message of a refusal whose code was invented later', () => {
    const result = parseServerMessage(sampleNamed('an error code invented later').frame);
    expect(result.ok).toBe(true);
    if (!result.ok || result.message.type !== 'response' || !('error' in result.message.outcome)) {
      throw new Error('the sample is a refused response');
    }

    expect(result.message.outcome.error.code).toBe('gpu_on_fire');
    expect(result.message.outcome.error.message).toBe('the graphics card is on fire');
  });

  it('keeps a detail it cannot read rather than losing the refusal with it', () => {
    const result = parseServerMessage(sampleNamed('an error detail invented later').frame);
    expect(result.ok).toBe(true);
    if (!result.ok || result.message.type !== 'response' || !('error' in result.message.outcome)) {
      throw new Error('the sample is a refused response');
    }

    const detail = result.message.outcome.error.detail;
    if (detail === undefined || isRecognisedErrorDetail(detail)) {
      throw new Error('a detail invented later is not one this build recognises');
    }

    expect(detail.unrecognised).toEqual({ detail: 'disk_full', free_bytes: 0 });
    expect(result.message.outcome.error.message).toBe(
      'the disk the recording was being written to is full',
    );
  });

  it('advertises a feature it has never heard of rather than refusing the welcome', () => {
    const result = parseServerMessage(sampleNamed('a feature invented later').frame);
    expect(result.ok).toBe(true);
    if (!result.ok || result.message.type !== 'welcome') {
      throw new Error('the sample is a welcome');
    }

    expect(result.message.features).toEqual(['recording', 'status_events', 'telepathy']);
  });

  it('ignores a field invented later rather than falling back to the catch-all', () => {
    const result = parseServerMessage(sampleNamed('a field invented later').frame);
    expect(result.ok).toBe(true);
    if (!result.ok || result.message.type !== 'event' || result.message.event === undefined) {
      throw new Error('the sample is a recognised event');
    }
    if (result.message.event !== 'status_changed' || result.message.status.state !== 'recording') {
      throw new Error('the sample is a recording status');
    }

    expect(result.message.status.elapsed_ms).toBe(4200);
    expect(result.message.status).not.toHaveProperty('gpu_temperature_c');
  });

  it('refuses to read a recorder state it does not know, rather than guessing', () => {
    // The asymmetry the compatibility policy turns on, and the one thing here
    // that must fail. An interface showing "idle" for a recorder in a state it
    // has never heard of would be telling the user something untrue.
    const result = parseServerMessage(
      sampleNamed('a recorder state invented later, inside a reply').frame,
    );
    expect(result.ok).toBe(false);
    if (result.ok) {
      throw new Error('an unknown state must not be readable');
    }
    expect(result.problem).toContain('paused');
  });
});
