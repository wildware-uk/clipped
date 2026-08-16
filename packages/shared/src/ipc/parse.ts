/**
 * Reading a frame into the types in `protocol.ts`.
 *
 * A type is a claim about what arrived; these functions are what checks it.
 * Without them the interface would be casting whatever came off the pipe and
 * calling it a `ServerMessage`, which is the same as having no types at all —
 * the first recorder that answered differently would produce `undefined` deep
 * inside a component rather than a refusal anybody could report.
 *
 * # Nothing here throws
 *
 * Every parser returns a {@link ParseResult}. A frame that cannot be read is a
 * fact about the connection, not an exception: it has to reach the interface as
 * something it can render, and an exception thrown out of a message handler
 * would take the subscription with it.
 *
 * # It reads what `crates/ipc` reads
 *
 * The rules below are not a reasonable interpretation of the protocol; they are
 * what `serde` does in `crates/ipc`, sample by sample, and
 * `conformance.test.ts` runs both sides over the same frames to prove it:
 *
 * - An unknown field is ignored, everywhere.
 * - An unknown error code, end reason, feature, stream or command name is kept
 *   as it arrived.
 * - An event whose name this build does not know — **or whose contents it
 *   cannot read** — becomes an {@link UnrecognisedEvent} holding the raw JSON,
 *   which is where `serde`'s untagged catch-all puts it. The same goes for an
 *   error detail.
 * - An unknown reply, outcome or envelope `type` fails the frame. Those are the
 *   tags that decide what a message *is*, and there is nothing to do with a
 *   message whose kind is unknown.
 * - An unknown recorder state is never rendered, and where it goes depends on
 *   what carried it. In a reply it fails the frame: the interface asked what
 *   the recorder was doing and has no answer it can show. In a `status_changed`
 *   event it fails the *event*, which then lands in the catch-all with every
 *   other event this build cannot read — so the subscription survives, and the
 *   state is still not shown. `crates/ipc` does exactly the same thing for the
 *   same reason, and the schema carries a sample of each.
 */

import type {
  ActiveRecording,
  BookmarkSummary,
  ClientMessage,
  ErrorDetail,
  ExportSummary,
  Hello,
  HotkeyBinding,
  HotkeyState,
  JsonObject,
  JsonValue,
  LibraryClip,
  LibraryEventLane,
  TrashListing,
  TrashedItem,
  LibraryEventMark,
  PluginDeclaration,
  PluginState,
  RefusedPlugin,
  LibraryGame,
  LibraryRecording,
  LibrarySession,
  LibrarySessionPage,
  ProtocolError,
  RecorderEvent,
  RecorderRequest,
  RecorderResponse,
  RecorderStatus,
  RecordingSummary,
  Reply,
  ReplaySummary,
  ScreenshotSummary,
  ServerMessage,
  Welcome,
} from './protocol';

/** What reading a frame produced, or why it produced nothing. */
export type ParseResult<T> =
  | {
      /** It was a message this build can read. */
      readonly ok: true;
      /** The message. */
      readonly message: T;
    }
  | {
      /** It was not. */
      readonly ok: false;
      /** One sentence saying what was wrong, for a log or a diagnostic panel. */
      readonly problem: string;
    };

/**
 * Why a frame could not be read.
 *
 * Internal: it is thrown while walking a frame and caught at the boundary, so
 * that the readers below can be written as though every field were there. It
 * never escapes this module.
 */
class Unreadable extends Error {}

/** Reads a frame the recorder sent. Never throws. */
export function parseServerMessage(frame: unknown): ParseResult<ServerMessage> {
  return attempt(() => readServerMessage(frame));
}

/** Reads a frame the desktop application sent. Never throws. */
export function parseClientMessage(frame: unknown): ParseResult<ClientMessage> {
  return attempt(() => readClientMessage(frame));
}

/**
 * Runs a reader and turns the one exception it can raise into a result.
 *
 * Anything else is a bug in this module rather than a fact about the frame, and
 * is left to propagate: swallowing it would report a protocol problem for what
 * was actually a mistake here.
 */
function attempt<T>(read: () => T): ParseResult<T> {
  try {
    return { ok: true, message: read() };
  } catch (thrown) {
    if (thrown instanceof Unreadable) {
      return { ok: false, problem: thrown.message };
    }
    throw thrown;
  }
}

function unreadable(problem: string): never {
  throw new Unreadable(problem);
}

function isObject(value: unknown): value is JsonObject {
  return typeof value === 'object' && value !== null && !Array.isArray(value);
}

function object(value: unknown, what: string): JsonObject {
  return isObject(value) ? value : unreadable(`${what} is not a JSON object`);
}

function stringField(source: JsonObject, name: string, what: string): string {
  const value = source[name];
  return typeof value === 'string' ? value : unreadable(`${what} has no \`${name}\` string`);
}

function numberField(source: JsonObject, name: string, what: string): number {
  const value = source[name];
  return typeof value === 'number' ? value : unreadable(`${what} has no \`${name}\` number`);
}

function optionalNumberField(source: JsonObject, name: string, what: string): number | undefined {
  const value = source[name];
  if (value === undefined || value === null) {
    return undefined;
  }
  return typeof value === 'number' ? value : unreadable(`${what}'s \`${name}\` is not a number`);
}

function booleanField(source: JsonObject, name: string, what: string): boolean {
  const value = source[name];
  return typeof value === 'boolean' ? value : unreadable(`${what} has no \`${name}\` boolean`);
}

function optionalStringField(source: JsonObject, name: string, what: string): string | undefined {
  const value = source[name];
  if (value === undefined || value === null) {
    return undefined;
  }
  return typeof value === 'string' ? value : unreadable(`${what}'s \`${name}\` is not a string`);
}

/**
 * A list of objects, each read by the same reader.
 *
 * A member that cannot be read fails the whole list rather than being dropped:
 * a library page missing the one recording this build could not parse would be
 * a page that looks complete and is not, which is worse than a refusal the user
 * can report.
 */
function arrayField<T>(
  value: JsonValue | undefined,
  what: string,
  read: (entry: JsonValue) => T,
): readonly T[] {
  if (!Array.isArray(value)) {
    unreadable(`${what} has no array where one was expected`);
  }
  return value.map(read);
}

function stringArrayField(source: JsonObject, name: string, what: string): readonly string[] {
  const value = source[name];
  if (!Array.isArray(value)) {
    unreadable(`${what} has no \`${name}\` array`);
  }
  return value.map((entry, index) =>
    typeof entry === 'string'
      ? entry
      : unreadable(`${what}'s \`${name}[${index}]\` is not a string`),
  );
}

function optionalStringArrayField(
  source: JsonObject,
  name: string,
  what: string,
): readonly string[] | undefined {
  return source[name] === undefined ? undefined : stringArrayField(source, name, what);
}

function numberArrayField(source: JsonObject, name: string, what: string): readonly number[] {
  const value = source[name];
  if (!Array.isArray(value)) {
    unreadable(`${what} has no \`${name}\` array`);
  }
  return value.map((entry, index) =>
    typeof entry === 'number'
      ? entry
      : unreadable(`${what}'s \`${name}[${index}]\` is not a number`),
  );
}

/**
 * Everything but the envelope's tag, which is what the catch-alls keep.
 *
 * `crates/ipc` sees the same thing: `serde` strips the tag it dispatched on
 * before handing the rest to the variant, so an unrecognised event kept here
 * and one kept there are the same object.
 */
function withoutType(frame: JsonObject): JsonValue {
  const rest: Record<string, JsonValue> = {};
  for (const [key, value] of Object.entries(frame)) {
    if (key !== 'type' && value !== undefined) {
      rest[key] = value;
    }
  }
  return rest;
}

function readClientMessage(frame: unknown): ClientMessage {
  const envelope = object(frame, 'a frame');
  const type = stringField(envelope, 'type', 'a frame');
  switch (type) {
    case 'hello':
      return { type: 'hello', ...readHello(envelope) };
    case 'request':
      return { type: 'request', ...readRequest(envelope) };
    default:
      return unreadable(`\`${type}\` is not a message this build knows how to read`);
  }
}

function readServerMessage(frame: unknown): ServerMessage {
  const envelope = object(frame, 'a frame');
  const type = stringField(envelope, 'type', 'a frame');
  switch (type) {
    case 'welcome':
      return { type: 'welcome', ...readWelcome(envelope) };
    case 'refused':
      return { type: 'refused', ...readProtocolError(envelope, 'a refusal') };
    case 'response':
      return { type: 'response', ...readResponse(envelope) };
    case 'event':
      return { type: 'event', ...readEvent(envelope) };
    default:
      return unreadable(`\`${type}\` is not a message this build knows how to read`);
  }
}

function readPeerIdentity(value: JsonValue | undefined, what: string) {
  const peer = object(value, what);
  return {
    name: stringField(peer, 'name', what),
    version: stringField(peer, 'version', what),
  };
}

function readHello(frame: JsonObject): Hello {
  const role = frame['role'];
  const streams = optionalStringArrayField(frame, 'streams', 'a handshake');
  return {
    protocol_version: numberField(frame, 'protocol_version', 'a handshake'),
    client: readPeerIdentity(frame['client'], "a handshake's client"),
    ...(role === undefined
      ? {}
      : { role: typeof role === 'string' ? role : unreadable('a handshake role is not a string') }),
    ...(streams === undefined ? {} : { streams }),
  };
}

function readWelcome(frame: JsonObject): Welcome {
  const streams = optionalStringArrayField(frame, 'streams', 'a welcome');
  return {
    protocol_version: numberField(frame, 'protocol_version', 'a welcome'),
    recorder: readPeerIdentity(frame['recorder'], "a welcome's recorder"),
    role: stringField(frame, 'role', 'a welcome'),
    features: stringArrayField(frame, 'features', 'a welcome'),
    ...(streams === undefined ? {} : { streams }),
  };
}

function readRequest(frame: JsonObject): RecorderRequest {
  const params = frame['params'];
  return {
    id: numberField(frame, 'id', 'a request'),
    command: stringField(frame, 'command', 'a request'),
    ...(params === undefined || params === null ? {} : { params }),
  };
}

function readResponse(frame: JsonObject): RecorderResponse {
  const id = numberField(frame, 'id', 'a response');
  const outcome = object(frame['outcome'], "a response's outcome");
  const tags = Object.keys(outcome);
  if (tags.length !== 1) {
    unreadable(`a response's outcome carries ${tags.length} tags, and it carries exactly one`);
  }

  switch (tags[0]) {
    case 'ok':
      return { id, outcome: { ok: readReply(outcome['ok']) } };
    case 'error':
      return { id, outcome: { error: readProtocolError(outcome['error'], 'a refusal') } };
    default:
      return unreadable(`\`${String(tags[0])}\` is neither \`ok\` nor \`error\``);
  }
}

function readReply(value: JsonValue | undefined): Reply {
  const reply = object(value, 'a reply');
  const tag = stringField(reply, 'reply', 'a reply');
  switch (tag) {
    case 'pong':
      return { reply: 'pong' };
    case 'status':
      return { reply: 'status', status: readStatus(reply['status']) };
    case 'recording_started':
      return {
        reply: 'recording_started',
        recording_id: stringField(reply, 'recording_id', 'a started recording'),
        output: stringField(reply, 'output', 'a started recording'),
      };
    case 'recording_stopped':
      return { reply: 'recording_stopped', summary: readSummary(reply['summary']) };
    case 'bookmark_added':
      return { reply: 'bookmark_added', bookmark: readBookmark(reply['bookmark']) };
    case 'screenshot_taken':
      return { reply: 'screenshot_taken', screenshot: readScreenshot(reply['screenshot']) };
    case 'replay_saved':
      return { reply: 'replay_saved', clip: readReplay(reply['clip']) };
    case 'library_sessions':
      return { reply: 'library_sessions', page: readSessionPage(reply['page']) };
    case 'library_games':
      return {
        reply: 'library_games',
        games: arrayField(reply['games'], 'a games list', readLibraryGame),
      };
    case 'library_events':
      return { reply: 'library_events', lane: readEventLane(reply['lane']) };
    case 'library_trash':
      return { reply: 'library_trash', trash: readTrashListing(reply['trash']) };
    case 'plugins':
      return {
        reply: 'plugins',
        installed: arrayField(reply['installed'], 'a plugin list', readPluginDeclaration),
        refused: arrayField(reply['refused'], 'a refused plugin list', readRefusedPlugin),
      };
    case 'recording_exported':
      return { reply: 'recording_exported', export: readExport(reply['export']) };
    case 'hotkeys':
      return {
        reply: 'hotkeys',
        hotkeys: arrayField(reply['hotkeys'], 'a hotkey list', readHotkeyBinding),
      };
    case 'shutting_down': {
      const finalising = reply['finalising'];
      return {
        reply: 'shutting_down',
        // Absent means nothing was being recorded, which is an answer rather
        // than a gap; a present one names a file the user should be told about.
        ...(finalising === undefined || finalising === null
          ? {}
          : { finalising: readActiveRecording(finalising) }),
      };
    }
    default:
      // No catch-all, deliberately: a reply this build cannot read is a command
      // whose outcome it does not know, and reporting that is the honest answer.
      return unreadable(`\`${tag}\` is not a reply this build knows`);
  }
}

function readActiveRecording(value: JsonValue | undefined): ActiveRecording {
  const recording = object(value, 'a recording');
  const replay = optionalNumberField(recording, 'replay_seconds', 'a recording');
  return {
    recording_id: stringField(recording, 'recording_id', 'a recording'),
    output: stringField(recording, 'output', 'a recording'),
    target: stringField(recording, 'target', 'a recording'),
    elapsed_ms: numberField(recording, 'elapsed_ms', 'a recording'),
    // Absent is "this recording keeps no replay buffer", which is an answer
    // rather than a gap: the recorder skips the field entirely for one that
    // was started without one.
    ...(replay === undefined ? {} : { replay_seconds: replay }),
  };
}

function readHotkeyBinding(value: JsonValue | undefined): HotkeyBinding {
  const binding = object(value, 'a hotkey');
  const what = 'a hotkey';
  const hotkey = optionalStringField(binding, 'hotkey', what);
  const unavailable = optionalStringField(binding, 'unavailable', what);
  return {
    action: stringField(binding, 'action', what),
    label: stringField(binding, 'label', what),
    // Absent means the action is bound to nothing, which is a row rather than
    // a gap: most actions start unbound.
    ...(hotkey === undefined ? {} : { hotkey }),
    state: readHotkeyState(binding['state']),
    handled: booleanField(binding, 'handled', what),
    ...(unavailable === undefined ? {} : { unavailable }),
  };
}

function readHotkeyState(value: JsonValue | undefined): HotkeyState {
  const state = object(value, 'a hotkey state');
  const tag = stringField(state, 'state', 'a hotkey state');
  switch (tag) {
    case 'unbound':
      return { state: 'unbound' };
    case 'registered':
      return { state: 'registered' };
    case 'conflict':
      return { state: 'conflict', reason: stringField(state, 'reason', 'a hotkey conflict') };
    default:
      // Not tolerated, for the reason a recorder state is not: the three states
      // this build knows all say the key either works or plainly does not, so a
      // fourth drawn as one of them would be a hotkey reported as working that
      // is not.
      return unreadable(`\`${tag}\` is not a hotkey state this build knows`);
  }
}

function readStatus(value: JsonValue | undefined): RecorderStatus {
  const status = object(value, 'a status');
  const state = stringField(status, 'state', 'a status');
  switch (state) {
    case 'idle':
      return { state: 'idle' };
    case 'recording':
      return { state: 'recording', ...readActiveRecording(status) };
    default:
      // The one unknown value that must not be tolerated. Showing "idle" for a
      // recorder in a state this build has never heard of would be a lie; not
      // being able to read the message is only a gap.
      //
      // This fails whatever is reading the status: the reply, and with it the
      // frame; or, inside a `status_changed` event, the event — which
      // {@link readEvent} then keeps as unrecognised rather than losing the
      // subscription over. Either way nothing is rendered from a state this
      // build cannot name, which is the whole of the promise.
      return unreadable(`\`${state}\` is not a recorder state this build knows`);
  }
}

function readSummary(value: JsonValue | undefined): RecordingSummary {
  const summary = object(value, 'a recording summary');
  const what = 'a recording summary';
  const sustained = optionalNumberField(summary, 'sustained_framerate', what);
  return {
    output: stringField(summary, 'output', what),
    duration_ms: numberField(summary, 'duration_ms', what),
    end_reason: stringField(summary, 'end_reason', what),
    frames_encoded: numberField(summary, 'frames_encoded', what),
    frames_skipped_for_rate: numberField(summary, 'frames_skipped_for_rate', what),
    frames_dropped_writer_behind: numberField(summary, 'frames_dropped_writer_behind', what),
    ...(sustained === undefined ? {} : { sustained_framerate: sustained }),
    encoder: stringField(summary, 'encoder', what),
    codec: stringField(summary, 'codec', what),
    width: numberField(summary, 'width', what),
    height: numberField(summary, 'height', what),
  };
}

function readBookmark(value: JsonValue | undefined): BookmarkSummary {
  const bookmark = object(value, 'a bookmark');
  const what = 'a bookmark';
  const label = optionalStringField(bookmark, 'label', what);
  const colour = optionalStringField(bookmark, 'colour', what);
  const duration = optionalNumberField(bookmark, 'duration_seconds', what);
  return {
    recording_id: stringField(bookmark, 'recording_id', what),
    at_seconds: numberField(bookmark, 'at_seconds', what),
    pressed_at_seconds: numberField(bookmark, 'pressed_at_seconds', what),
    lead_seconds: numberField(bookmark, 'lead_seconds', what),
    ...(label === undefined ? {} : { label }),
    ...(colour === undefined ? {} : { colour }),
    ...(duration === undefined ? {} : { duration_seconds: duration }),
    bookmarks_file: stringField(bookmark, 'bookmarks_file', what),
    bookmarks_in_recording: numberField(bookmark, 'bookmarks_in_recording', what),
  };
}

function readScreenshot(value: JsonValue | undefined): ScreenshotSummary {
  const screenshot = object(value, 'a screenshot');
  const what = 'a screenshot';
  const recording = optionalStringField(screenshot, 'recording_id', what);
  const at = optionalNumberField(screenshot, 'at_seconds', what);
  return {
    path: stringField(screenshot, 'path', what),
    format: stringField(screenshot, 'format', what),
    width: numberField(screenshot, 'width', what),
    height: numberField(screenshot, 'height', what),
    bytes: numberField(screenshot, 'bytes', what),
    ...(recording === undefined ? {} : { recording_id: recording }),
    ...(at === undefined ? {} : { at_seconds: at }),
  };
}

function readReplay(value: JsonValue | undefined): ReplaySummary {
  const clip = object(value, 'a replay clip');
  const what = 'a replay clip';
  return {
    path: stringField(clip, 'path', what),
    recording_id: stringField(clip, 'recording_id', what),
    requested_seconds: numberField(clip, 'requested_seconds', what),
    duration_seconds: numberField(clip, 'duration_seconds', what),
    source_start_seconds: numberField(clip, 'source_start_seconds', what),
    source_end_seconds: numberField(clip, 'source_end_seconds', what),
    leading_slack_seconds: numberField(clip, 'leading_slack_seconds', what),
    complete: booleanField(clip, 'complete', what),
    shortfall_seconds: numberField(clip, 'shortfall_seconds', what),
    bytes: numberField(clip, 'bytes', what),
  };
}

function readExport(value: JsonValue | undefined): ExportSummary {
  const summary = object(value, 'an export');
  const what = 'an export';
  const losses = optionalStringArrayField(summary, 'losses', what);
  return {
    source: stringField(summary, 'source', what),
    destination: stringField(summary, 'destination', what),
    duration_ms: numberField(summary, 'duration_ms', what),
    packets: numberField(summary, 'packets', what),
    bytes: numberField(summary, 'bytes', what),
    elapsed_ms: numberField(summary, 'elapsed_ms', what),
    lossless: booleanField(summary, 'lossless', what),
    // Absent is "nothing was lost", which is an answer rather than a gap: the
    // recorder skips the field entirely when the copy holds everything.
    ...(losses === undefined ? {} : { losses }),
  };
}

function readSessionPage(value: JsonValue | undefined): LibrarySessionPage {
  const page = object(value, 'a library page');
  const cursor = optionalStringField(page, 'next_cursor', 'a library page');
  return {
    sessions: arrayField(page['sessions'], 'a library page', readLibrarySession),
    // Absent is the end of the library, which is an answer rather than a gap.
    ...(cursor === undefined ? {} : { next_cursor: cursor }),
  };
}

function readLibrarySession(value: JsonValue | undefined): LibrarySession {
  const session = object(value, 'a session');
  const what = 'a session';
  const gameId = optionalStringField(session, 'game_id', what);
  const gameName = optionalStringField(session, 'game_name', what);
  const endedAt = optionalStringField(session, 'ended_at', what);
  const endReason = optionalStringField(session, 'end_reason', what);
  return {
    session_id: stringField(session, 'session_id', what),
    ...(gameId === undefined ? {} : { game_id: gameId }),
    ...(gameName === undefined ? {} : { game_name: gameName }),
    started_at: stringField(session, 'started_at', what),
    ...(endedAt === undefined ? {} : { ended_at: endedAt }),
    ...(endReason === undefined ? {} : { end_reason: endReason }),
    favourite: booleanField(session, 'favourite', what),
    recordings: arrayField(session['recordings'], what, readLibraryRecording),
    clips: arrayField(session['clips'], what, readLibraryClip),
  };
}

function readLibraryRecording(value: JsonValue | undefined): LibraryRecording {
  const recording = object(value, 'a recording');
  const what = 'a recording';
  const endedAt = optionalStringField(recording, 'ended_at', what);
  const outcome = optionalStringField(recording, 'outcome', what);
  const endReason = optionalStringField(recording, 'end_reason', what);
  const duration = optionalNumberField(recording, 'duration_seconds', what);
  const width = optionalNumberField(recording, 'width', what);
  const height = optionalNumberField(recording, 'height', what);
  const size = optionalNumberField(recording, 'size_bytes', what);
  const missing = optionalStringField(recording, 'missing_since', what);
  return {
    recording_id: numberField(recording, 'recording_id', what),
    session_index: numberField(recording, 'session_index', what),
    path: stringField(recording, 'path', what),
    started_at: stringField(recording, 'started_at', what),
    ...(endedAt === undefined ? {} : { ended_at: endedAt }),
    ...(outcome === undefined ? {} : { outcome }),
    ...(endReason === undefined ? {} : { end_reason: endReason }),
    ...(duration === undefined ? {} : { duration_seconds: duration }),
    ...(width === undefined ? {} : { width }),
    ...(height === undefined ? {} : { height }),
    ...(size === undefined ? {} : { size_bytes: size }),
    // The field the whole read exists for: absent means the file is there, and
    // present means the screen has to say it has gone.
    ...(missing === undefined ? {} : { missing_since: missing }),
    favourite: booleanField(recording, 'favourite', what),
    tags: stringArrayField(recording, 'tags', what),
  };
}

function readLibraryClip(value: JsonValue | undefined): LibraryClip {
  const clip = object(value, 'a clip');
  const what = 'a clip';
  const title = optionalStringField(clip, 'title', what);
  const duration = optionalNumberField(clip, 'duration_seconds', what);
  const size = optionalNumberField(clip, 'size_bytes', what);
  const missing = optionalStringField(clip, 'missing_since', what);
  return {
    clip_id: numberField(clip, 'clip_id', what),
    path: stringField(clip, 'path', what),
    ...(title === undefined ? {} : { title }),
    created_at: stringField(clip, 'created_at', what),
    ...(duration === undefined ? {} : { duration_seconds: duration }),
    ...(size === undefined ? {} : { size_bytes: size }),
    ...(missing === undefined ? {} : { missing_since: missing }),
    favourite: booleanField(clip, 'favourite', what),
    tags: stringArrayField(clip, 'tags', what),
  };
}

/**
 * One installed plugin.
 *
 * `network` is required rather than defaulted: an absent list and an empty one
 * would otherwise be indistinguishable, and they are "the recorder did not say"
 * and "it declares none" — the second of which a screen must state rather than
 * leave blank.
 */
function readPluginDeclaration(value: JsonValue | undefined): PluginDeclaration {
  const plugin = object(value, 'a plugin');
  const what = 'a plugin';
  return {
    id: stringField(plugin, 'id', what),
    name: stringField(plugin, 'name', what),
    version: stringField(plugin, 'version', what),
    description: stringField(plugin, 'description', what),
    network: stringArrayField(plugin, 'network', what),
    enforcement: stringField(plugin, 'enforcement', what),
    state: readPluginState(plugin['state']),
  };
}

/** What a plugin's state is, and what changed when consent has lapsed. */
function readPluginState(value: JsonValue | undefined): PluginState {
  const state = object(value, 'a plugin state');
  const tag = stringField(state, 'state', 'a plugin state');
  switch (tag) {
    case 'enabled':
    case 'not-enabled':
    case 'turned-off':
      return { state: tag };
    case 'needs-consent-again':
      return {
        state: tag,
        agreed_to: stringField(state, 'agreed_to', 'a lapsed consent'),
        now_declares: stringField(state, 'now_declares', 'a lapsed consent'),
      };
    default:
      unreadable(`\`${tag}\` is not a plugin state this build knows`);
  }
}

/** Something that is not a usable plugin, and why. */
function readRefusedPlugin(value: JsonValue | undefined): RefusedPlugin {
  const refused = object(value, 'a refused plugin');
  return {
    directory: stringField(refused, 'directory', 'a refused plugin'),
    reason: stringField(refused, 'reason', 'a refused plugin'),
  };
}

/**
 * One recording's marks.
 *
 * `marks` is required rather than defaulted: an absent array and an empty one
 * would otherwise be indistinguishable, and they are "the recorder did not
 * answer this" and "there are none" — different things to draw.
 */
function readEventLane(value: JsonValue | undefined): LibraryEventLane {
  const lane = object(value, 'an event lane');
  return { marks: arrayField(lane['marks'], 'a marks list', readEventMark) };
}

/**
 * One mark.
 *
 * `kind` is read as a plain string and checked against nothing. A kind added
 * after this build shipped, and a plugin's namespaced custom name, both arrive
 * here, and a reader that refused one would delete exactly the marks that have
 * to survive.
 */
function readEventMark(value: JsonValue | undefined): LibraryEventMark {
  const mark = object(value, 'a mark');
  const what = 'a mark';
  return {
    recording: stringField(mark, 'recording', what),
    at: numberField(mark, 'at', what),
    kind: stringField(mark, 'kind', what),
    source: stringField(mark, 'source', what),
  };
}

/** What is in the trash, and what emptying it would take. */
function readTrashListing(value: JsonValue | undefined): TrashListing {
  const listing = object(value, 'a trash listing');
  const what = 'a trash listing';
  return {
    items: arrayField(listing['items'], 'a trash list', readTrashedItem),
    total_items: numberField(listing, 'total_items', what),
    total_bytes: numberField(listing, 'total_bytes', what),
    directory: stringField(listing, 'directory', what),
  };
}

/**
 * One thing waiting in the trash.
 *
 * `expires_at` and `size_bytes` are optional and stay optional: a trash whose
 * retention this build cannot work out, and a file the index never measured,
 * are both real states, and inventing a date or a zero would be a screen saying
 * something nobody measured.
 */
function readTrashedItem(value: JsonValue | undefined): TrashedItem {
  const item = object(value, 'a trashed item');
  const what = 'a trashed item';
  const expires = optionalStringField(item, 'expires_at', what);
  const size = optionalNumberField(item, 'size_bytes', what);
  return {
    kind: stringField(item, 'kind', what),
    id: numberField(item, 'id', what),
    path: stringField(item, 'path', what),
    original_path: stringField(item, 'original_path', what),
    deleted_at: stringField(item, 'deleted_at', what),
    ...(expires === undefined ? {} : { expires_at: expires }),
    ...(size === undefined ? {} : { size_bytes: size }),
    dependent_clips: numberField(item, 'dependent_clips', what),
  };
}

function readLibraryGame(value: JsonValue | undefined): LibraryGame {
  const game = object(value, 'a game');
  const what = 'a game';
  const id = optionalStringField(game, 'game_id', what);
  const name = optionalStringField(game, 'name', what);
  const firstSeen = optionalStringField(game, 'first_seen_at', what);
  const lastPlayed = optionalStringField(game, 'last_played_at', what);
  return {
    // Both absent is the row for sittings the catalogue would not attribute,
    // and it is a row rather than a fault.
    ...(id === undefined ? {} : { game_id: id }),
    ...(name === undefined ? {} : { name }),
    ...(firstSeen === undefined ? {} : { first_seen_at: firstSeen }),
    ...(lastPlayed === undefined ? {} : { last_played_at: lastPlayed }),
    sessions: numberField(game, 'sessions', what),
    recordings: numberField(game, 'recordings', what),
    clips: numberField(game, 'clips', what),
    favourites: numberField(game, 'favourites', what),
    bytes: numberField(game, 'bytes', what),
    missing: numberField(game, 'missing', what),
  };
}

function readProtocolError(value: JsonValue | undefined, what: string): ProtocolError {
  const error = object(value, what);
  const detail = error['detail'];
  return {
    code: stringField(error, 'code', what),
    message: stringField(error, 'message', what),
    ...(detail === undefined || detail === null ? {} : { detail: readErrorDetail(detail) }),
  };
}

/**
 * A detail this build knows, or the JSON it arrived as.
 *
 * The catch-all is what keeps an unfamiliar detail from costing the refusal its
 * code and its message, which is the whole of the additive half of the
 * compatibility policy.
 */
function readErrorDetail(value: JsonValue): ErrorDetail {
  if (isObject(value)) {
    try {
      switch (value['detail']) {
        case 'unsupported_protocol_version':
          return {
            detail: 'unsupported_protocol_version',
            requested: numberField(value, 'requested', 'a version refusal'),
            supported: numberArrayField(value, 'supported', 'a version refusal'),
            recorder_version: stringField(value, 'recorder_version', 'a version refusal'),
          };
        case 'not_implemented':
          return {
            detail: 'not_implemented',
            subsystem: stringField(value, 'subsystem', 'a missing subsystem'),
            milestone: stringField(value, 'milestone', 'a missing subsystem'),
            tracking_issue: numberField(value, 'tracking_issue', 'a missing subsystem'),
          };
        default:
          break;
      }
    } catch (thrown) {
      if (!(thrown instanceof Unreadable)) {
        throw thrown;
      }
      // A tag this build knows carrying contents it cannot read is still kept
      // rather than failing the refusal, which is where `serde`'s untagged
      // catch-all puts it too.
    }
  }

  return { unrecognised: value };
}

/**
 * An event this build knows, or the JSON it arrived as.
 *
 * Anything unreadable lands in the catch-all rather than failing the frame: an
 * events connection that dropped because the recorder sent something new would
 * be the exact failure the compatibility policy exists to prevent.
 */
function readEvent(frame: JsonObject): RecorderEvent {
  try {
    switch (frame['event']) {
      case 'status_changed':
        return { event: 'status_changed', status: readStatus(frame['status']) };
      case 'recording_failed':
        return {
          event: 'recording_failed',
          recording_id: stringField(frame, 'recording_id', 'a failed recording'),
          error: readProtocolError(frame['error'], 'a failed recording'),
        };
      default:
        break;
    }
  } catch (thrown) {
    if (!(thrown instanceof Unreadable)) {
      throw thrown;
    }
  }

  return { unrecognised: withoutType(frame) };
}
