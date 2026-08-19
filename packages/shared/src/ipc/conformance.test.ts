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
  AdapterSummary,
  CaptureAccount,
  CaptureMethodChange,
  CodecSummary,
  Diagnostics,
  EffectiveSetting,
  DiagnosticsReply,
  EncoderAccount,
  EncoderSummary,
  ActiveRecording,
  AddBookmarkParams,
  ApplySettingsParams,
  AudioDevice,
  AudioDevices,
  AudioDevicesReply,
  MicrophoneLevel,
  MicrophoneLevelParams,
  MicrophoneLevelReply,
  SetStartAtLoginParams,
  StartAtLogin,
  StartAtLoginReply,
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
  OpenPlaybackParams,
  OpenPreviewParams,
  PlaybackOpenedReply,
  PlaybackStream,
  PlaybackTrack,
  ExportProgress,
  ExportProgressEvent,
  Preview,
  PreviewKind,
  PreviewOpenedReply,
  PreviewPicture,
  PreviewState,
  PreviewTrack,
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
  RestoredItem,
  RestoredReply,
  TrashedItem,
  TrashEmptiedReply,
  FavouritedReply,
  LockedReply,
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
  CategoryUsage,
  GetStorageParams,
  ProtectedGroup,
  RecordingList,
  StorageLimits,
  StorageRecording,
  StorageReply,
  StorageReport,
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
  PREVIEW_KINDS,
  PREVIEW_STATES,
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
  preview_kind: enumeration<PreviewKind>(PREVIEW_KINDS, false),
  preview_state: enumeration<PreviewState>(PREVIEW_STATES, false),
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
    replay: 'optional',
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
    end_reason: 'optional',
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
    locked: 'optional',
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
    locked: 'optional',
    protected: 'optional',
    tags: 'required',
  }),
  library_clip: fields<LibraryClip>({
    clip_id: 'required',
    path: 'optional',
    title: 'optional',
    created_at: 'required',
    duration_seconds: 'optional',
    size_bytes: 'optional',
    missing_since: 'optional',
    favourite: 'required',
    tags: 'required',
  }),
  trashed_item: fields<TrashedItem>({
    kind: 'required',
    id: 'required',
    // Optional, and this is what holds the hand-written mirror to it: an item
    // with no file is a clip nothing has exported, which was never anywhere and
    // has nowhere to be put back to (issue #593).
    path: 'optional',
    original_path: 'optional',
    deleted_at: 'required',
    expires_at: 'optional',
    size_bytes: 'optional',
    dependent_clips: 'required',
  }),
  get_storage: fields<GetStorageParams>({
    // Optional, and this is what holds the mirror to it: a request with no
    // parameters asks about the limits that are configured, and one carrying
    // them asks what saving those would delete. A mirror that made this
    // required could not put the first question at all.
    limits: 'optional',
  }),
  storage_limits: fields<StorageLimits>({
    // All three optional, and absent means no limit of that kind — which is
    // what Clipped ships with, so an empty object is the ordinary reading
    // rather than a frame with fields missing.
    maximum_usage_bytes: 'optional',
    minimum_free_space_bytes: 'optional',
    maximum_age_days: 'optional',
  }),
  storage_recording: fields<StorageRecording>({
    recording_id: 'required',
    path: 'required',
    size_bytes: 'required',
    started_at: 'required',
    // Optional, and absent is the reading that matters: a recording nothing
    // protects is one a sweep may take.
    protected_because: 'optional',
  }),
  recording_list: fields<RecordingList>({
    total: 'required',
    total_bytes: 'required',
    recordings: 'required',
  }),
  protected_group: fields<ProtectedGroup>({
    label: 'required',
    recordings: 'required',
    bytes: 'required',
  }),
  category_usage: fields<CategoryUsage>({
    category: 'required',
    bytes: 'required',
  }),
  storage_report: fields<StorageReport>({
    recordings_directory: 'required',
    trash_directory: 'required',
    usage_bytes: 'required',
    by_category: 'required',
    free_bytes: 'required',
    capacity_bytes: 'required',
    limits: 'required',
    // Required, deliberately. It is the difference between what is happening
    // and what would happen if somebody saved a limit they have not saved, and
    // a mirror that let it default would draw one as the other (AGENTS.md
    // section 56).
    proposed: 'required',
    would_delete: 'required',
    still_over_limit: 'required',
    protected: 'required',
    largest: 'required',
  }),
  restored_item: fields<RestoredItem>({
    kind: 'required',
    id: 'required',
    path: 'optional',
    file_restored: 'required',
    renamed: 'required',
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
  open_playback: fields<OpenPlaybackParams>({
    source: 'required',
    audio_track: 'optional',
  }),
  playback_stream: fields<PlaybackStream>({
    path: 'required',
    audio_track: 'optional',
    audio_tracks: 'optional',
    prepared: 'optional',
  }),
  playback_track: fields<PlaybackTrack>({
    index: 'required',
    name: 'optional',
    language: 'optional',
    default: 'optional',
  }),
  open_preview: fields<OpenPreviewParams>({
    source: 'required',
    kind: 'required',
    buckets: 'optional',
  }),
  preview: fields<Preview>({
    kind: 'required',
    state: 'required',
    picture: 'optional',
    tracks: 'optional',
    reason: 'optional',
  }),
  preview_picture: fields<PreviewPicture>({
    media_type: 'required',
    bytes: 'required',
    width: 'required',
    height: 'required',
    at_seconds: 'required',
    blank: 'optional',
  }),
  preview_track: fields<PreviewTrack>({
    index: 'required',
    name: 'optional',
    sample_rate: 'required',
    channels: 'required',
    duration_seconds: 'required',
    peaks: 'optional',
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
  export_progress: fields<ExportProgress>({
    source: 'required',
    destination: 'required',
    written_ms: 'required',
    total_ms: 'optional',
    packets: 'required',
    bytes: 'required',
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
  'reply.storage': fields<StorageReply>({
    reply: 'required',
    storage: 'required',
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
  'reply.locked': fields<LockedReply>({
    reply: 'required',
    lock: 'required',
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
  'reply.playback_opened': fields<PlaybackOpenedReply>({
    reply: 'required',
    playback: 'required',
  }),
  'reply.preview_opened': fields<PreviewOpenedReply>({
    reply: 'required',
    preview: 'required',
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
  diagnostics: fields<Diagnostics>({
    capture: 'optional',
    encoders: 'required',
    settings: 'optional',
  }),
  effective_setting: fields<EffectiveSetting>({
    setting: 'required',
    value: 'required',
    source: 'required',
  }),
  capture_account: fields<CaptureAccount>({
    setting: 'required',
    started_with: 'required',
    current: 'required',
    changes: 'required',
  }),
  capture_method_change: fields<CaptureMethodChange>({
    from: 'required',
    to: 'required',
    restart: 'required',
    trigger: 'required',
    reason: 'required',
  }),
  encoder_account: fields<EncoderAccount>({
    probed: 'required',
    detected_at: 'optional',
    elapsed_ms: 'required',
    adapters: 'required',
    encoders: 'required',
  }),
  adapter_summary: fields<AdapterSummary>({
    description: 'required',
    vendor: 'required',
    kind: 'required',
    video_memory_bytes: 'required',
    driver_version: 'optional',
    captures: 'required',
  }),
  encoder_summary: fields<EncoderSummary>({
    encoder: 'required',
    label: 'required',
    available: 'required',
    unavailable: 'optional',
    implemented: 'required',
    adapter: 'optional',
    asked: 'required',
    codecs: 'required',
  }),
  codec_summary: fields<CodecSummary>({
    codec: 'required',
    supported: 'optional',
    max_width: 'optional',
    max_height: 'optional',
    max_framerate_1080p: 'optional',
    inferred: 'required',
  }),
  'reply.diagnostics': fields<DiagnosticsReply>({ reply: 'required', diagnostics: 'required' }),
  setting_entry: fields<SettingEntry>({
    key: 'required',
    label: 'required',
    value: 'required',
    overridden: 'required',
    choices: 'optional',
    accepted: 'required',
    applies: 'required',
    unavailable: 'optional',
    not_yet_in_force: 'optional',
  }),
  settings_view: fields<SettingsView>({ file: 'required', settings: 'required' }),
  'reply.settings': fields<SettingsReply>({ reply: 'required', settings: 'required' }),
  audio_device: fields<AudioDevice>({ name: 'required', is_default: 'required' }),
  audio_devices: fields<AudioDevices>({ microphones: 'required' }),
  'reply.audio_devices': fields<AudioDevicesReply>({ reply: 'required', devices: 'required' }),
  microphone_level_request: fields<MicrophoneLevelParams>({ microphone: 'required' }),
  microphone_level: fields<MicrophoneLevel>({
    peak: 'required',
    device: 'optional',
    muted: 'optional',
  }),
  'reply.microphone_level': fields<MicrophoneLevelReply>({ reply: 'required', level: 'required' }),
  start_at_login: fields<StartAtLogin>({
    enabled: 'required',
    location: 'required',
    command: 'optional',
    missing_executable: 'optional',
  }),
  set_start_at_login: fields<SetStartAtLoginParams>({ enabled: 'required' }),
  'reply.start_at_login': fields<StartAtLoginReply>({
    reply: 'required',
    start_at_login: 'required',
  }),
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
  'event.export_progress': fields<ExportProgressEvent>({
    event: 'required',
    export: 'required',
  }),
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
    name: 'set_lock',
    params: 'set_lock',
    reply: 'reply.locked',
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
    name: 'open_playback',
    params: 'open_playback',
    reply: 'reply.playback_opened',
    available_in_this_build: true,
  },
  {
    name: 'open_preview',
    params: 'open_preview',
    reply: 'reply.preview_opened',
    available_in_this_build: true,
  },
  {
    name: 'get_hotkeys',
    params: null,
    reply: 'reply.hotkeys',
    available_in_this_build: true,
  },
  {
    name: 'get_diagnostics',
    params: null,
    reply: 'reply.diagnostics',
    available_in_this_build: true,
  },
  {
    name: 'get_storage',
    params: 'get_storage',
    reply: 'reply.storage',
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
    name: 'get_microphone_level',
    params: 'microphone_level_request',
    reply: 'reply.microphone_level',
    available_in_this_build: true,
  },
  {
    name: 'get_start_at_login',
    params: null,
    reply: 'reply.start_at_login',
    available_in_this_build: true,
  },
  {
    name: 'set_start_at_login',
    params: 'set_start_at_login',
    reply: 'reply.start_at_login',
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
    case 'locked':
      // One discriminant: whether the lock changed, and whether the sweep will
      // leave the thing alone, are both fields rather than shapes.
      return 'locked';
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
    case 'diagnostics':
      // Whether a recording is being captured is part of the path: a mirror
      // that dropped `capture` would reach the same discriminant for a recorder
      // that has a backend to name and one that has none, and which of those it
      // is decides whether the screen has a capture row to draw at all.
      return reply.diagnostics.capture === undefined ? 'diagnostics' : 'diagnostics.capturing';
    case 'storage':
      // Whether the limits were proposed is part of the path: a mirror that
      // dropped `proposed` would reach the same discriminant for a report of
      // what is configured and a dry run of what somebody is about to
      // configure, and the whole difference between them is whether the
      // deletions listed have been agreed to (AGENTS.md section 56).
      return reply.storage.proposed ? 'storage.proposed' : 'storage';
    case 'settings':
      // One discriminant: a settings view with nothing in it is the same shape
      // carrying nothing, and `settings` is always present.
      return 'settings';
    case 'audio_devices':
      // The same. A machine with no microphone is an empty list rather than a
      // different reply.
      return 'audio_devices';
    case 'microphone_level':
      // One discriminant: a device that is not there and one that is silent are
      // the same shape with a field left out, and which is which is
      // `MicrophoneLevel.device` rather than a different reply.
      return 'microphone_level';
    case 'start_at_login':
      // Three paths, because they are the three things the window says
      // differently: off, on, and on but pointing at an executable that is no
      // longer there. A mirror that dropped either field would reach the same
      // discriminant for a working startup arrangement and a broken one
      // (issue #308).
      if (!reply.start_at_login.enabled) {
        return 'start_at_login.off';
      }
      return reply.start_at_login.missing_executable === undefined
        ? 'start_at_login.on'
        : 'start_at_login.missing';
    case 'recording_exported':
      // Whether the copy is complete is part of the path, because it is the one
      // thing the window has to say differently: a mirror that dropped
      // `lossless` would reach the same discriminant for an MP4 that holds the
      // whole recording and one that quietly does not.
      return reply.export.lossless ? 'recording_exported' : 'recording_exported.lossy';
    case 'playback_opened':
      // Whether a copy had to be made is part of the path: it is the difference
      // between an answer that cost nothing and one that read the whole
      // recording, and a mirror that dropped `prepared` would reach the same
      // discriminant for both.
      return `playback_opened.${reply.playback.prepared === true ? 'prepared' : 'as_recorded'}`;
    case 'preview_opened':
      // Both the kind and the state are part of the path, which is issue #448's
      // second criterion expressed as a discriminant: a mirror that carried the
      // picture and dropped the state would reach the same answer for a
      // thumbnail that is here and one there will never be, and a mirror that
      // dropped the kind would reach it for peaks and a picture alike.
      return `preview_opened.${reply.preview.kind}.${reply.preview.state}`;
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
    case 'export_progress':
      // Whether the recording said how long it was is part of the path, because
      // it is the one thing a window draws differently: a total is a percentage
      // and no total is an unbounded indication. A mirror that read a missing
      // `total_ms` as zero would otherwise reach the same answer as one that
      // kept it absent.
      return `export_progress.${event.export.total_ms === undefined ? 'unmeasured' : 'measured'}`;
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

describe('a clip nothing has exported yet', () => {
  /**
   * Issue #591's wire half, asserted on the frame itself.
   *
   * The frame is the recorder's own — `crates/ipc/src/schema.rs` builds it out
   * of the real `LibraryClip` and serialises it with the real `serde`, and it
   * is committed here — so what this reads is what goes down the pipe. #576 and
   * #586 were both fields whose absence a *parsed* reply could not distinguish
   * from their presence, which is why the key is checked on the raw JSON before
   * anything in this package touches it.
   */
  const frame = sampleNamed('a clip nothing has exported yet, which has no file').frame;

  it('is sent with no `path` key at all, rather than a null or a blank one', () => {
    const sent = frame as {
      outcome: { ok: { page: { sessions: { clips: Record<string, unknown>[] }[] } } };
    };
    const clip = sent.outcome.ok.page.sessions[0]?.clips[0];
    if (clip === undefined) {
      throw new Error('the sample carries a sitting with a clip in it');
    }

    expect(Object.keys(clip)).not.toContain('path');
    expect(Object.keys(clip)).not.toContain('missing_since');
    expect(clip['clip_id']).toBe(3);
  });

  it('is read as a clip with no file rather than refusing the whole page', () => {
    const result = parseServerMessage(frame);
    expect(
      result.ok,
      result.ok ? '' : `a sitting with an unexported highlight was rejected: ${result.problem}`,
    ).toBe(true);
    if (!result.ok || result.message.type !== 'response' || !('ok' in result.message.outcome)) {
      throw new Error('the sample is a successful response');
    }
    const reply = result.message.outcome.ok;
    if (reply.reply !== 'library_sessions') {
      throw new Error('the sample is a page of the library');
    }

    const session = reply.page.sessions[0];
    if (session === undefined) {
      throw new Error('the sample carries a sitting');
    }

    // Nothing else in the sitting is lost by tolerating the pathless clip.
    expect(session.recordings).toHaveLength(1);
    expect(session.recordings[0]?.path).toBe('D:\\clips\\cs2-20260811-201400-1.mkv');

    expect(session.clips).toHaveLength(1);
    const clip = session.clips[0];
    // Absent, not `''`: an empty string is a file name a screen would open.
    expect(clip).not.toHaveProperty('path');
    expect(clip).not.toHaveProperty('missing_since');
    expect(clip?.title).toBe('Ace on Mirage');
  });
});

describe('a thing in the trash that has no file', () => {
  /**
   * Issue #593's wire half, asserted on the frames themselves.
   *
   * The frames are the recorder's own -- `crates/ipc/src/schema.rs` builds them
   * out of the real `TrashedItem` and `RestoredItem` and serialises them with
   * the real `serde`, and they are committed here -- so what this reads is what
   * goes down the pipe. #576 and #586 were both fields whose absence a *parsed*
   * reply could not distinguish from their presence, which is why the keys are
   * checked on the raw JSON before anything in this package touches it.
   */
  const listing = sampleNamed('a clip with no file waiting in the trash').frame;
  const restored = sampleNamed('a clip with no file put back, which brings no file with it').frame;

  it('is listed with no `path` and no `original_path` key at all', () => {
    const sent = listing as {
      outcome: { ok: { trash: { items: Record<string, unknown>[] } } };
    };
    const item = sent.outcome.ok.trash.items[0];
    if (item === undefined) {
      throw new Error('the sample carries a trash with something in it');
    }

    // Absent, not `''`: an empty string is a file name a screen would open,
    // and nothing that never had a file has anywhere to be put back to.
    expect(Object.keys(item)).not.toContain('path');
    expect(Object.keys(item)).not.toContain('original_path');
    expect(item['kind']).toBe('clip');
    expect(item['id']).toBe(7);
  });

  it('is read as an item with no file rather than refusing the whole trash', () => {
    const result = parseServerMessage(listing);
    expect(
      result.ok,
      result.ok ? '' : `a trash holding a pathless clip was rejected: ${result.problem}`,
    ).toBe(true);
    if (!result.ok || result.message.type !== 'response' || !('ok' in result.message.outcome)) {
      throw new Error('the sample is a successful response');
    }
    const reply = result.message.outcome.ok;
    if (reply.reply !== 'library_trash') {
      throw new Error('the sample is a trash listing');
    }

    expect(reply.trash.items).toHaveLength(1);
    const item = reply.trash.items[0];
    expect(item).not.toHaveProperty('path');
    expect(item).not.toHaveProperty('original_path');
    expect(item?.deleted_at).toBe('2026-08-15T09:00:00+01:00');
  });

  it('comes back out of the trash with no file, rather than not coming back', () => {
    const sent = restored as { outcome: { ok: { restored: Record<string, unknown> } } };
    expect(Object.keys(sent.outcome.ok.restored)).not.toContain('path');

    const result = parseServerMessage(restored);
    expect(
      result.ok,
      result.ok ? '' : `a restored pathless clip was rejected: ${result.problem}`,
    ).toBe(true);
    if (!result.ok || result.message.type !== 'response' || !('ok' in result.message.outcome)) {
      throw new Error('the sample is a successful response');
    }
    const reply = result.message.outcome.ok;
    if (reply.reply !== 'restored') {
      throw new Error('the sample is a restored item');
    }

    expect(reply.restored).not.toHaveProperty('path');
    expect(reply.restored.file_restored).toBe(false);
    expect(reply.restored.id).toBe(7);
  });
});

describe('a sitting that has ended', () => {
  /**
   * Issue #625, at the layer that decides whether a window can say anything.
   *
   * A recording somebody stopped explains itself in the `recording_summary` the
   * stop is answered with. A recording that ended *by itself* has no reply, and
   * this event is the only thing the recorder ever sends about it — so a reader
   * that dropped `end_reason` from each file would leave a window able to name
   * the file and unable to say why it stopped, and a sitting cut short by a
   * window being dragged to a new size would look exactly like one that ran to
   * the end (ADR 0012).
   *
   * Not covered by the discriminant check above, which reads the *sitting's*
   * reason: both files here belong to one sitting that ended `game-exited`, and
   * a reader that kept that and lost the two below would reach the same answer.
   */
  const frame = sampleNamed('a sitting ending, with the files it produced').frame;

  it('says why each of its files ended, and not only why the sitting did', () => {
    const result = parseServerMessage(frame);
    expect(result.ok, result.ok ? '' : `a sitting that ended was rejected: ${result.problem}`).toBe(
      true,
    );
    if (!result.ok || result.message.type !== 'event' || result.message.event !== 'session_ended') {
      throw new Error('the sample is a session_ended event');
    }

    const { session } = result.message;
    expect(session.end_reason).toBe('game-exited');
    expect(session.recordings.map((recording) => recording.end_reason)).toEqual([
      'stopped',
      // The word this whole field exists for. Hyphenated, because this is the
      // sidecar's and the index's vocabulary rather than the underscored
      // `EndReason` a `stop_recording` is answered with.
      'target-resized',
    ]);
  });

  it('leaves the reason off a file it has none for, rather than inventing one', () => {
    // The last entry of an open sitting is the file being written now, which has
    // not ended and therefore has no reason to have ended. An empty string here
    // would be a screen drawing a sentence about a recording still running.
    const running = parseServerMessage(
      sampleNamed('the status of a recorder that is recording').frame,
    );
    if (
      !running.ok ||
      running.message.type !== 'response' ||
      !('ok' in running.message.outcome) ||
      running.message.outcome.ok.reply !== 'status'
    ) {
      throw new Error('the sample is a status');
    }

    const status = running.message.outcome.ok.status;
    if (status.state !== 'recording') {
      throw new Error('the sample is a recording status');
    }
    const open = status.session?.recordings.at(-1);
    expect(open?.output).toBeTypeOf('string');
    expect(open).not.toHaveProperty('end_reason');
  });
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
