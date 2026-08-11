/**
 * The framing, tested against the rules `docs/ipc.md` states.
 *
 * The interesting cases are the ones a byte stream produces on its own: half a
 * frame, two frames in one buffer, and a length prefix that must be refused
 * before anything is allocated for it.
 */

import { describe, expect, it } from 'vitest';

import { LENGTH_PREFIX_BYTES, MAX_FRAME_BYTES, decodeFrame, encodeFrame } from './frame';
import type { ClientMessage } from './protocol';

const hello: ClientMessage = {
  type: 'hello',
  protocol_version: 1,
  client: { name: 'clipped-desktop', version: '0.1.0' },
  role: 'control',
};

function prefixed(length: number, payload: Uint8Array = new Uint8Array()): Uint8Array {
  const bytes = new Uint8Array(LENGTH_PREFIX_BYTES + payload.length);
  new DataView(bytes.buffer).setUint32(0, length, true);
  bytes.set(payload, LENGTH_PREFIX_BYTES);
  return bytes;
}

describe('framing', () => {
  it('writes the length little-endian, ahead of the payload', () => {
    const frame = encodeFrame(hello);
    const payload = JSON.stringify(hello);

    expect(frame.length).toBe(LENGTH_PREFIX_BYTES + payload.length);
    expect([...frame.subarray(0, LENGTH_PREFIX_BYTES)]).toEqual([payload.length, 0, 0, 0]);
    expect(new TextDecoder().decode(frame.subarray(LENGTH_PREFIX_BYTES))).toBe(payload);
  });

  it('reads back what it wrote', () => {
    const read = decodeFrame(encodeFrame(hello));
    expect(read.kind).toBe('frame');
    if (read.kind !== 'frame') {
      throw new Error('a frame it just wrote should be readable');
    }
    expect(read.payload).toEqual(hello);
    expect(read.bytes).toBe(encodeFrame(hello).length);
  });

  it('leaves the second message in the buffer alone', () => {
    const first = encodeFrame(hello);
    const both = new Uint8Array(first.length * 2);
    both.set(first);
    both.set(first, first.length);

    const read = decodeFrame(both);
    expect(read.kind === 'frame' && read.bytes).toBe(first.length);
  });

  it('says a frame is incomplete rather than reading past the end of it', () => {
    const frame = encodeFrame(hello);
    expect(decodeFrame(frame.subarray(0, 2)).kind).toBe('incomplete');
    expect(decodeFrame(frame.subarray(0, frame.length - 1)).kind).toBe('incomplete');
  });

  it('refuses an oversized length prefix without allocating for it', () => {
    // The hostile case: a peer that declares four gigabytes gets a refusal, not
    // four gigabytes. Nothing here reads past the prefix, which is why this
    // test can pass a buffer with no payload in it at all.
    const read = decodeFrame(prefixed(4_000_000_000));
    expect(read).toEqual({ kind: 'too-large', declared: 4_000_000_000 });
    expect(decodeFrame(prefixed(MAX_FRAME_BYTES + 1)).kind).toBe('too-large');
  });

  it('reports a payload that is not JSON rather than throwing', () => {
    const payload = new TextEncoder().encode('{not json');
    expect(decodeFrame(prefixed(payload.length, payload)).kind).toBe('malformed');
  });

  it('refuses to write a message that would not fit in a frame', () => {
    const enormous: ClientMessage = {
      type: 'request',
      id: 1,
      command: 'start_recording',
      params: { output: 'x'.repeat(MAX_FRAME_BYTES) },
    };
    expect(() => encodeFrame(enormous)).toThrow(RangeError);
  });
});
