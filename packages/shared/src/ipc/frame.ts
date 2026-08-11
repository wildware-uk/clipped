/**
 * Where one message ends and the next begins.
 *
 * ```text
 * ┌───────────────┬─────────────────────────────────┐
 * │ length: u32   │ payload: `length` bytes of JSON │
 * │ little-endian │                                 │
 * └───────────────┴─────────────────────────────────┘
 * ```
 *
 * A pipe is a byte stream: two messages can arrive in one read and one message
 * can arrive in three. Nothing about the payload delimits it — JSON has no
 * terminator and a newline is legal inside a string, so a reader that looked
 * for one could be confused by a window title.
 *
 * These functions are the framing and nothing else: bytes in, bytes out, no
 * connection anywhere. Opening the pipe, holding it and reading from it is
 * issue #217, and belongs above this.
 *
 * # The limit is checked before anything is allocated
 *
 * A length prefix is an instruction from the other end of a pipe to allocate
 * memory. {@link MAX_FRAME_BYTES} is checked before a payload byte is touched,
 * for the same reason `crates/ipc` checks it: the largest message this protocol
 * has is a few hundred bytes, so a frame anywhere near the limit means
 * something is wrong.
 */

import type { ClientMessage, ServerMessage } from './protocol';

/**
 * The largest frame either side will send or accept.
 *
 * A bound on damage rather than a budget to spend. `conformance.test.ts` checks
 * it against the recorder's own constant.
 */
export const MAX_FRAME_BYTES = 1024 * 1024;

/** The width of the length prefix, in bytes. Little-endian. */
export const LENGTH_PREFIX_BYTES = 4;

/** What reading the front of a buffer produced. */
export type FrameRead =
  | {
      /** A whole frame was there. */
      readonly kind: 'frame';
      /** Its payload, parsed from JSON but not yet checked against the protocol. */
      readonly payload: unknown;
      /** How many bytes it occupied, prefix included. */
      readonly bytes: number;
    }
  | {
      /**
       * The buffer stops part way through a frame.
       *
       * Not a fault: it is what a byte stream does. Read more and ask again.
       */
      readonly kind: 'incomplete';
    }
  | {
      /** The peer declared a frame larger than {@link MAX_FRAME_BYTES}. */
      readonly kind: 'too-large';
      /** What the length prefix claimed. Nothing was read or allocated for it. */
      readonly declared: number;
    }
  | {
      /** The bytes inside the frame were not JSON. */
      readonly kind: 'malformed';
      /** One sentence saying what was wrong. */
      readonly problem: string;
    };

/**
 * Wraps a message in its length prefix.
 *
 * @throws RangeError if the message does not fit in a frame. That is a fault in
 * the caller rather than a fact about the connection — no message this protocol
 * defines comes close to the limit — so it is not a {@link FrameRead}.
 */
export function encodeFrame(message: ClientMessage | ServerMessage): Uint8Array {
  const payload = new TextEncoder().encode(JSON.stringify(message));
  if (payload.length > MAX_FRAME_BYTES) {
    throw new RangeError(
      `a ${payload.length}-byte message does not fit in the ${MAX_FRAME_BYTES}-byte frame limit`,
    );
  }

  const frame = new Uint8Array(LENGTH_PREFIX_BYTES + payload.length);
  new DataView(frame.buffer).setUint32(0, payload.length, true);
  frame.set(payload, LENGTH_PREFIX_BYTES);
  return frame;
}

/**
 * Reads the frame at the front of a buffer, if a whole one is there.
 *
 * The caller keeps the remainder: a stream reader appends what it read and
 * calls this until it says `incomplete`.
 */
export function decodeFrame(bytes: Uint8Array): FrameRead {
  if (bytes.length < LENGTH_PREFIX_BYTES) {
    return { kind: 'incomplete' };
  }

  const declared = new DataView(bytes.buffer, bytes.byteOffset).getUint32(0, true);
  if (declared > MAX_FRAME_BYTES) {
    return { kind: 'too-large', declared };
  }
  if (bytes.length < LENGTH_PREFIX_BYTES + declared) {
    return { kind: 'incomplete' };
  }

  const payload = bytes.subarray(LENGTH_PREFIX_BYTES, LENGTH_PREFIX_BYTES + declared);
  try {
    return {
      kind: 'frame',
      payload: JSON.parse(new TextDecoder('utf-8', { fatal: true }).decode(payload)) as unknown,
      bytes: LENGTH_PREFIX_BYTES + declared,
    };
  } catch (thrown) {
    return {
      kind: 'malformed',
      problem: thrown instanceof Error ? thrown.message : 'the frame was not JSON',
    };
  }
}
