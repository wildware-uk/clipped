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

/** The protocol version this build of the interface speaks. */
export const PROTOCOL_VERSION = 1;

/**
 * Every protocol version this build can hold a conversation in.
 *
 * A recorder may speak more than one at once; the interface speaks exactly one
 * and is told, in the refusal, what the recorder speaks instead.
 */
export const SUPPORTED_PROTOCOL_VERSIONS = [1] as const;

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
export const FEATURES = ['recording', 'status_events'] as const;

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

/** Every command the protocol defines, including the four no build performs yet. */
export const COMMANDS = [
  'ping',
  'get_status',
  'start_recording',
  'stop_recording',
  'save_replay',
  'add_bookmark',
  'take_screenshot',
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
  'recording_failed',
  'too_many_connections',
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
export const RECORDER_STATES = ['idle', 'recording'] as const;

/** What the recorder is doing. There is no unrecognised state. */
export type RecorderState = (typeof RECORDER_STATES)[number];

/** The tags a response's outcome is carried under. */
export const OUTCOMES = ['ok', 'error'] as const;

/** The replies this build knows. */
export const REPLIES = ['pong', 'status', 'recording_started', 'recording_stopped'] as const;

/** The name of a reply. Closed: a reply nobody can read is a failed command. */
export type ReplyName = (typeof REPLIES)[number];

/** The events this build knows. */
export const EVENTS = ['status_changed', 'recording_failed'] as const;

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

/**
 * What a command produced.
 *
 * No unrecognised variant: a reply the interface cannot read is a command whose
 * outcome it does not know, which it must report rather than absorb.
 */
export type Reply = PongReply | StatusReply | RecordingStartedReply | RecordingStoppedReply;

/** Nothing is being recorded. */
export interface IdleStatus {
  /** The tag. */
  readonly state: 'idle';
}

/** A recording is in progress. */
export interface RecordingStatus {
  /** The tag. */
  readonly state: 'recording';
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
}

/**
 * What the recorder is doing right now.
 *
 * The one union here with no catch-all. See the file header.
 */
export type RecorderStatus = IdleStatus | RecordingStatus;

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
  /** The subsystem in the user's words: "the replay buffer". */
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
export type RecorderEvent = StatusChangedEvent | RecordingFailedEvent | UnrecognisedEvent;

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
): event is StatusChangedEvent | RecordingFailedEvent {
  return event.event !== undefined;
}
