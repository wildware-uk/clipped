import type { LibraryClip, LibrarySession } from '@clipped/shared';
import { describe, expect, it, vi } from 'vitest';

import { readEvents, recentClips } from './library';

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

describe('the newest clips across recent sittings', () => {
  /** A sitting with the clips given, and nothing else that matters here. */
  function sitting(id: string, clips: readonly Partial<LibraryClip>[]): LibrarySession {
    return {
      session_id: id,
      game_id: 'cs2',
      game_name: `Game ${id}`,
      started_at: '2026-08-11T20:14:00+01:00',
      favourite: false,
      recordings: [],
      clips: clips.map((clip, index) => ({
        clip_id: Number(`${id.replace(/\D/gu, '') || '0'}${String(index)}`),
        created_at: '2026-08-11T20:20:00+01:00',
        favourite: false,
        tags: [],
        ...clip,
      })),
    };
  }

  it('gathers clips from every sitting, newest first', () => {
    const gathered = recentClips(
      [
        sitting('a', [{ created_at: '2026-08-11T10:00:00+01:00', path: 'old.mkv' }]),
        sitting('b', [
          { created_at: '2026-08-13T10:00:00+01:00', path: 'newest.mkv' },
          { created_at: '2026-08-12T10:00:00+01:00', path: 'middle.mkv' },
        ]),
      ],
      10,
    );

    expect(gathered.map((each) => each.clip.path)).toEqual(['newest.mkv', 'middle.mkv', 'old.mkv']);
  });

  /*
   * A clip can be cut from an old recording long after the sitting ended.
   * Ordering by the sitting would file it under a date months old and bury it,
   * which is the opposite of what a "recently clipped" list is for.
   */
  it('orders on when the clip was made, not on when the sitting was', () => {
    const old = {
      ...sitting('a', [{ created_at: '2026-08-20T10:00:00+01:00', path: 'cut-today.mkv' }]),
      started_at: '2026-01-01T10:00:00+01:00',
    };
    const recent = {
      ...sitting('b', [{ created_at: '2026-08-19T10:00:00+01:00', path: 'cut-yesterday.mkv' }]),
      started_at: '2026-08-19T09:00:00+01:00',
    };

    expect(recentClips([recent, old], 10).map((each) => each.clip.path)).toEqual([
      'cut-today.mkv',
      'cut-yesterday.mkv',
    ]);
  });

  it('keeps each clip with the sitting it came from', () => {
    const gathered = recentClips([sitting('a', [{ path: 'one.mkv' }])], 10);

    expect(gathered[0]?.session.game_name).toBe('Game a');
  });

  it('takes no more than it was asked for', () => {
    const many = sitting(
      'a',
      Array.from({ length: 20 }, (_unused, index) => ({
        created_at: `2026-08-${String(index + 1).padStart(2, '0')}T10:00:00+01:00`,
      })),
    );

    expect(recentClips([many], 3)).toHaveLength(3);
    expect(recentClips([many], 0)).toHaveLength(0);
    expect(recentClips([many], -1)).toHaveLength(0);
  });

  it('is empty when nothing has been clipped', () => {
    expect(recentClips([sitting('a', [])], 5)).toEqual([]);
    expect(recentClips([], 5)).toEqual([]);
  });
});
