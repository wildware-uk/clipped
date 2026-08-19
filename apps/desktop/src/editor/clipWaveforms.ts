import { useCallback, useEffect, useMemo, useState } from 'react';

import { recordingsIn } from './document';
import type { PeaksOf } from './lanePeaks';
import { asProblem } from '../library';
import { PREVIEWS, readPreview, type WaveformView } from '../preview';
import { recorderCanDo, type RecorderLinkState } from '../useRecorderLink';

/**
 * The peaks of every recording an open clip draws on (issue #66).
 *
 * # Why not `useWaveform`
 *
 * `../preview.ts`'s hook reads **one** recording, which is what the playback
 * screen has. A clip has as many as it has sources — one ordinarily, more once
 * recordings are joined (issue #88) — and a hook cannot be called in a loop
 * whose length changes between renders. So this is one hook over a list, and it
 * uses the same `readPreview` round trip underneath; nothing about the *answer*
 * is read differently here, and nothing about the *picture* is drawn
 * differently, which is `../waveformOutline.ts`'s job for both screens.
 *
 * # Why the answers are keyed on the recording and not on the clip
 *
 * Because that is what they are answers about. A recording's peaks do not
 * depend on which clip is being edited, so opening a second clip that draws on
 * the same file finds the answer already here rather than asking again — and
 * there is no stale-answer problem of the kind `useThumbnail` has to compare
 * its way out of, because no key can ever mean two things.
 *
 * # Asking is what makes one
 *
 * The recorder answers `pending` *and* queues the work, so opening a clip is
 * what puts its recordings at the front of the waveform queue. Nothing here
 * polls for a `pending` to become `ready`: the queue drops its oldest entry
 * rather than its newest, so a screen asking again on a timer would push the
 * work it wants out of its own queue. The peaks appear the next time the clip
 * is opened, and the note under the timeline says so.
 */

/**
 * How many buckets of peaks the editor asks for, per recording.
 *
 * `MAX_PREVIEW_BUCKETS` in `crates/ipc/src/preview.rs`, which is the most the
 * recorder will answer with and what it clamps a larger request to.
 *
 * The editor asks for the most rather than for a width, which is the opposite
 * of the playback screen's constant and for a reason that is worth writing
 * down: the buckets are spread over the **whole recording**, and a lane draws
 * only the part of it a segment uses. A clip of eight seconds cut from a
 * two-hour session gets its share of the answer and no more, so asking for the
 * width of the lane would leave that lane with a single bucket in it. Asking
 * for the maximum is the finest slice this protocol can produce; a request that
 * named a *range* of the recording would do better and is not a thing the
 * command takes (issue #657).
 */
export const EDITOR_BUCKETS = 4096;

/** What one round trip produced, in the two ways it can end. */
type Answer = Extract<WaveformView, { state: 'answered' } | { state: 'refused' }>;

/**
 * Reads the peaks of every recording `clip` draws on.
 *
 * `clip` is the stored text of the edit document, which is what crosses the
 * boundary (`./clipDocument.ts`). It is read here only for the list of
 * recordings in it — through `readEditDocument`, the one reader this window
 * has, rather than a second one (AGENTS.md section 55).
 *
 * `link` is what the recorder said it can do. A recorder from before issue #448
 * has no `open_preview` and would refuse every one of these by name, so it is
 * asked nothing at all and every lane reads `unasked` — which the editor draws
 * as a lane with no waveform and a note saying the recorder is the reason
 * (AGENTS.md sections 27 and 45).
 */
export function useClipWaveforms(clip: string | null, link: RecorderLinkState | null): PeaksOf {
  const [answers, setAnswers] = useState<ReadonlyMap<string, Answer>>(new Map());
  const canAsk = recorderCanDo(link, PREVIEWS);
  /*
   * Held so the effect below runs when the clip's *recordings* change rather
   * than on every render: a fresh array would be a fresh dependency, and this
   * effect opens a pipe per entry.
   */
  const recordings = useMemo(() => (clip === null ? [] : recordingsIn(clip)), [clip]);

  useEffect(() => {
    if (!canAsk) {
      return;
    }
    let current = true;
    const remember = (recording: string, answer: Answer): void => {
      if (current) {
        setAnswers((held) => new Map(held).set(recording, answer));
      }
    };
    for (const recording of recordings) {
      readPreview(recording, 'waveform', EDITOR_BUCKETS)
        .then((preview) => {
          remember(recording, { state: 'answered', preview });
        })
        .catch((thrown: unknown) => {
          // The recorder's own sentence rather than one invented here
          // (AGENTS.md section 15). "Clipped could not ask" and "there are no
          // peaks for that file" send somebody looking in different places.
          remember(recording, { state: 'refused', problem: asProblem(thrown) });
        });
    }
    return (): void => {
      current = false;
    };
  }, [recordings, canAsk]);

  return useCallback(
    (recording: string): WaveformView => {
      if (!canAsk) {
        return { state: 'unasked' };
      }
      return answers.get(recording) ?? { state: 'asking' };
    },
    [answers, canAsk],
  );
}
