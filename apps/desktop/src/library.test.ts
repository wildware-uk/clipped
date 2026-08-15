import { describe, expect, it, vi } from 'vitest';

import { readEvents } from './library';

const invoke = vi.hoisted(() => vi.fn());
vi.mock('@tauri-apps/api/core', () => ({ invoke }));

describe('readEvents', () => {
  it('asks for one recording and answers with its marks', async () => {
    invoke.mockResolvedValueOnce({
      marks: [{ recording: '1', at: 4_000_000_000, kind: 'kill', source: 'cs2' }],
    });

    const lane = await readEvents('1');

    expect(invoke).toHaveBeenCalledWith('library_events', { recording: '1' });
    expect(lane.marks).toHaveLength(1);
    expect(lane.marks[0]?.at).toBe(4_000_000_000);
  });

  it('carries a kind this build has never met rather than dropping it', async () => {
    // The property `docs/ipc.md` and the conformance suite both hold the
    // protocol to: a kind added after this build shipped, and a plugin's
    // namespaced custom name, are marks that have to be drawn. Anything here
    // that narrowed `kind` to a known set would delete exactly those.
    invoke.mockResolvedValueOnce({
      marks: [
        {
          recording: '7',
          at: 9_500_000_000,
          kind: 'acme-cs2.flashbang_blinded_five',
          source: 'acme-cs2',
        },
      ],
    });

    const lane = await readEvents('7');

    expect(lane.marks[0]?.kind).toBe('acme-cs2.flashbang_blinded_five');
  });

  it('answers a recording with no events as an empty lane', async () => {
    // Empty is "there are none". "Nobody asked" is not calling this at all,
    // and the two are drawn differently — so this must not become null or a
    // rejection on the way through.
    invoke.mockResolvedValueOnce({ marks: [] });

    const lane = await readEvents('9');

    expect(lane.marks).toEqual([]);
  });
});
