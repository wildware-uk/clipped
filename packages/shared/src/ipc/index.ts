/**
 * The recorder control protocol in TypeScript: the messages, and the reading of
 * them.
 *
 * `docs/ipc.md` is the specification, `crates/ipc` the implementation, and
 * these files a mirror that a test holds against both on every run — see
 * `conformance.test.ts`. What is deliberately not here is the connection: the
 * pipe, the handshake sequence and the request-and-reply loop are issue #217.
 */

export type {
  ClientMessage,
  CommandName,
  ConnectionRole,
  ErrorCode,
  ErrorDetail,
  ErrorOutcome,
  EventMessage,
  EventStream,
  Extensible,
  Feature,
  Hello,
  HelloMessage,
  IdleStatus,
  JsonObject,
  JsonValue,
  KnownCommandName,
  KnownConnectionRole,
  KnownEndReason,
  KnownErrorCode,
  KnownErrorDetailName,
  KnownEventName,
  KnownEventStream,
  KnownFeature,
  LibraryClip,
  LibraryGame,
  LibraryGamesReply,
  LibraryRecording,
  LibrarySession,
  LibrarySessionPage,
  LibrarySessionsParams,
  LibrarySessionsReply,
  NotImplementedDetail,
  OkOutcome,
  Outcome,
  PeerIdentity,
  PongReply,
  ProtocolError,
  RecorderEvent,
  RecorderRequest,
  RecorderResponse,
  RecorderState,
  RecorderStatus,
  RecordingFailedEvent,
  RecordingStartedReply,
  RecordingStatus,
  RecordingStoppedReply,
  RecordingSummary,
  RefusedMessage,
  Reply,
  ReplyName,
  RequestMessage,
  ResponseMessage,
  ServerMessage,
  StartRecordingParams,
  StatusChangedEvent,
  StatusReply,
  StopRecordingParams,
  UnrecognisedErrorDetail,
  UnrecognisedEvent,
  UnsupportedProtocolVersionDetail,
  WelcomeMessage,
  EndReason,
} from './protocol';

export {
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
  MAX_CONCURRENT_CONNECTIONS,
  OUTCOMES,
  PROTOCOL_VERSION,
  RECORDER_STATES,
  REPLIES,
  SERVER_MESSAGE_TYPES,
  SUPPORTED_PROTOCOL_VERSIONS,
  hasFeature,
  isKnownErrorCode,
  isRecognisedErrorDetail,
  isRecognisedEvent,
} from './protocol';

export type { ParseResult } from './parse';
export { parseClientMessage, parseServerMessage } from './parse';

export type { FrameRead } from './frame';
export { LENGTH_PREFIX_BYTES, MAX_FRAME_BYTES, decodeFrame, encodeFrame } from './frame';
