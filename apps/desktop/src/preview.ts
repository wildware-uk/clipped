import type { Feature, Preview, PreviewKind, PreviewPicture } from '@clipped/shared';
import { invoke } from '@tauri-apps/api/core';
import { useEffect, useState } from 'react';

import { asProblem, type LibraryProblem } from './library';

/**
 * A recording's thumbnail, and the peaks of its sound, in this window
 * (issue #448).
 *
 * # Where the picture comes from
 *
 * Not from the disk. Both are generated in the background and cached in
 * Clipped's own directory (`docs/thumbnails.md`, `docs/waveforms.md`), and this
 * window can open neither: it has no file-system permission, no asset scope and
 * may not link `clipped-library`. So it asks the Tauri host, which asks the
 * recorder, and what comes back is the picture itself as base64 in the reply —
 * drawn from a `data:` URI, which `tauri.conf.json`'s `img-src` already permits.
 * `crates/ipc/src/preview.rs` argues that choice against an asset scope at
 * length; the short of it is that a scope would have carried the picture and
 * left the peaks needing a second mechanism.
 *
 * # Three answers, and none of them is an error
 *
 * A {@link Preview} is `pending`, `ready` or `unavailable`, and only the last is
 * a refusal. "Not generated yet" is the ordinary state of a recording that has
 * just been written, so a screen that drew it as a failure would put a broken
 * tile over every new recording — and one that drew "there will never be one"
 * as "not yet" would promise a picture that is not coming. Telling the two apart
 * on screen is issue #448's second acceptance criterion, and it is `Thumbnail`'s
 * whole job.
 *
 * A round trip that *fails* is a fourth thing again, and separate from all
 * three: the recorder refused, or there was no recorder to ask. It is carried as
 * a {@link LibraryProblem} rather than folded into `unavailable`, because
 * "Clipped could not ask" and "there is no picture of that file" send somebody
 * looking in different places (AGENTS.md section 15).
 *
 * # Asking is what makes one
 *
 * The recorder answers `pending` *and* queues the work, so drawing a row is what
 * puts that recording at the front of the queue. That is why nothing here polls
 * for a `pending` to become `ready`: a screen that asked again on a timer would
 * queue the same recording once a second, and the queue drops its oldest entry
 * rather than its newest. The picture appears the next time the row is drawn.
 */

/** The capability a recorder advertises when it can answer for a preview. */
export const PREVIEWS: Feature = 'previews';

/**
 * Asks the recorder for one recording's thumbnail or waveform.
 *
 * `buckets` is the waveform's resolution and is meaningless for a thumbnail,
 * which has one stored size. It goes over as `null` when it is not given, for
 * the reason `library.ts`'s reads pass `null`: the Rust side declares it
 * `Option<u32>`, and `undefined` is not a JSON value — an argument left off the
 * object is an argument Tauri never sees.
 */
export async function readPreview(
  source: string,
  kind: PreviewKind,
  buckets?: number,
): Promise<Preview> {
  return invoke<Preview>('recording_preview', { source, kind, buckets: buckets ?? null });
}

/**
 * A picture, as the address an `<img>` takes.
 *
 * The media type comes from the answer rather than being assumed here, so the
 * day the generator writes WebP instead of JPEG (`docs/thumbnails.md` argues it
 * both ways) nothing in the window changes. Exported on its own because it is
 * the one piece of this file that is pure, and the one that would be wrong in a
 * way no rendered test would name: a URI missing its `;base64` draws nothing and
 * reports nothing.
 */
export function pictureUri(picture: PreviewPicture): string {
  return `data:${picture.media_type};base64,${picture.bytes}`;
}

/**
 * How many preview round trips this window will have in flight at once.
 *
 * Not tuning: a bound the recorder imposes. `RecorderLink::call` opens a
 * **fresh pipe connection for every command** — a control connection is
 * request-then-response in strict alternation, so it cannot share the one the
 * events arrive on (`crates/ipc/src/supervisor/link.rs`) — and the recorder
 * serves `MAX_CONCURRENT_CONNECTIONS`, which is eight
 * (`crates/ipc/src/server.rs`, `docs/ipc.md`). Beyond that it refuses with
 * `too_many_connections` and closes the connection.
 *
 * A thumbnail is one round trip **per row**, and a Library page mounting a
 * dozen rows at once is the first thing this window has ever done that could
 * ask for a dozen connections in the same instant. The damage would not be
 * confined to the tiles either: the eight are shared with everything else being
 * asked at that moment, so a page of thumbnails could take the answer out of a
 * Show more, a Keep or an Export.
 *
 * Three, because the link already holds a connection of its own for events and
 * the eight have to have room in them for the command somebody just pressed a
 * button for. It bounds how fast a page fills in, not how much of it does.
 */
export const CONCURRENT_PREVIEWS = 3;

/**
 * A recording somebody has asked about, and that has not been answered yet.
 *
 * Keyed by the recording, so that two rows drawing the same file — and the two
 * mounts StrictMode gives every row — share one round trip rather than racing
 * to make two of them.
 */
interface Asked {
  /** How many mounted rows still want it. */
  wanted: number;
  /** The single round trip they all read. */
  readonly answer: Promise<Preview>;
  /** Sends it. `null` once it has been sent, which is what "in flight" means. */
  send: (() => void) | null;
}

/** Every recording asked about and not yet answered, in the order asked. */
const asked = new Map<string, Asked>();

/** How many are on the wire right now. */
let inFlight = 0;

/**
 * Starts as many waiting requests as there are slots, newest first.
 *
 * Newest first for the reason `crates/background`'s own queue drops its
 * *oldest* entry rather than its newest: the newest is what somebody has just
 * scrolled to, and a row that has been on screen while thirty others went past
 * it is the one that can wait. A slot spent on a row nobody is looking at is a
 * slot the visible ones are queued behind.
 */
function sendWhatFits(): void {
  while (inFlight < CONCURRENT_PREVIEWS) {
    // A `Map` keeps insertion order, so the last one still holding a `send` is
    // the most recently asked for.
    const next = [...asked.values()].reverse().find((entry) => entry.send !== null);
    const send = next?.send;
    if (next === undefined || send === null || send === undefined) {
      return;
    }
    next.send = null;
    inFlight += 1;
    send();
  }
}

/**
 * Asks for one recording's thumbnail, behind {@link CONCURRENT_PREVIEWS}.
 *
 * The promise is shared by every row wanting the same recording and settles
 * when the recorder answers. A request still waiting when the last row wanting
 * it goes away is **dropped** rather than sent: by the time a slot came free it
 * would be a picture for a row that has scrolled off, fetched ahead of one that
 * has scrolled on.
 */
function askForThumbnail(source: string): Promise<Preview> {
  const already = asked.get(source);
  if (already !== undefined) {
    already.wanted += 1;
    return already.answer;
  }

  let send: (() => void) | null = null;
  const answer = new Promise<Preview>((resolve, reject) => {
    // Assigned synchronously: a promise executor runs before the constructor
    // returns, so `send` is set by the time it is put in the map below.
    send = (): void => {
      readPreview(source, 'thumbnail')
        .then(resolve, reject)
        .finally(() => {
          inFlight -= 1;
          asked.delete(source);
          sendWhatFits();
        });
    };
  });

  asked.set(source, { wanted: 1, answer, send });
  sendWhatFits();
  return answer;
}

/** One row has stopped wanting `source`; drops it if nobody else does. */
function stopWanting(source: string): void {
  const entry = asked.get(source);
  if (entry === undefined) {
    return;
  }
  entry.wanted -= 1;
  // Only one that has not been sent can be dropped. One already on the wire is
  // finishing whatever happens here, and its answer is what the next row asking
  // for the same file would have got anyway.
  if (entry.wanted <= 0 && entry.send !== null) {
    asked.delete(source);
  }
}

/**
 * Where a row stands with one recording's thumbnail.
 *
 * Four cases rather than a preview and two flags, and shaped like
 * `LibraryRead` in `library.ts` for the same reason: a screen has to draw every
 * one of them and there is no combination of them that is not one of the four.
 * A pair of booleans beside a nullable answer would have three states that
 * cannot happen and one — "answered, with nothing in it" — that a row would
 * have to draw as something, which is where a fabricated state gets in
 * (AGENTS.md section 27).
 */
export type ThumbnailView =
  /** Nothing was asked, because this recorder cannot answer. */
  | { readonly state: 'unasked' }
  /** A round trip is in flight. */
  | { readonly state: 'asking' }
  /** The recorder answered, in one of its own three states. */
  | { readonly state: 'answered'; readonly preview: Preview }
  /** The asking itself failed, which is not one of those three. */
  | { readonly state: 'refused'; readonly problem: LibraryProblem };

/** What one round trip produced, and which recording it was about. */
interface Answer {
  /** The recording it was an answer about. */
  readonly source: string;
  /**
   * How it came out — either of the two ways a round trip can end.
   *
   * The two halves of {@link ThumbnailView} that can only be reached by asking,
   * so that reading the view is a comparison and never a reconstruction.
   */
  readonly result:
    | { readonly state: 'answered'; readonly preview: Preview }
    | { readonly state: 'refused'; readonly problem: LibraryProblem };
}

/**
 * Reads `source`'s thumbnail, and follows the row as it is recycled.
 *
 * `null` asks for nothing, which is what a caller passes when the recorder does
 * not advertise `previews` — a hook cannot be called conditionally, and a list
 * that called this only for some rows would be the bug that rule exists to
 * prevent. Passing `null` there rather than asking and swallowing the refusal is
 * what keeps an older recorder from being asked once per row and refusing once
 * per row, which on a page of twenty-five sittings is a lot of nothing.
 *
 * # Why the state is one answer rather than three flags
 *
 * The answer carries **what it was an answer to**, and everything the row reads
 * is derived by comparing that against the recording being drawn now. The
 * Library's table is virtualised (`virtualWindow.ts`): a `<tr>` scrolled out of
 * view is a `<tr>` React reuses for a different recording, so a reply that
 * arrives after the swap is a reply about a file this row is no longer showing.
 * Comparing is what discards it. A flag set in the effect body would have to be
 * kept in step by hand, and the case where it is not is a row drawn with another
 * recording's picture in it — a wrong picture, which is worse than none.
 *
 * # Why it queues rather than asking
 *
 * A row asking the moment it mounts is a row opening a pipe the moment it
 * mounts, and the recorder serves eight of those between everything this window
 * does. {@link CONCURRENT_PREVIEWS} is that bound, and unmounting gives up a
 * request that has not left yet.
 */
export function useThumbnail(source: string | null): ThumbnailView {
  const [answer, setAnswer] = useState<Answer | null>(null);

  useEffect(() => {
    if (source === null) {
      return;
    }
    let current = true;
    askForThumbnail(source)
      .then((preview) => {
        if (current) {
          setAnswer({ source, result: { state: 'answered', preview } });
        }
      })
      .catch((thrown: unknown) => {
        if (current) {
          // The recorder's own sentence rather than one invented here, for the
          // reason `playback.ts` keeps one (AGENTS.md section 15).
          setAnswer({ source, result: { state: 'refused', problem: asProblem(thrown) } });
        }
      });
    return (): void => {
      current = false;
      stopWanting(source);
    };
  }, [source]);

  if (source === null) {
    return { state: 'unasked' };
  }
  /** The answer about the recording this row is drawing now, if it has arrived. */
  const current = answer !== null && answer.source === source ? answer : null;
  return current === null ? { state: 'asking' } : current.result;
}

/**
 * The heading above a preview that could not be asked for.
 *
 * Separate from `describeProblem`'s "Your library could not be read", because
 * the library was read — that is where the row came from. What failed is one
 * picture, and saying otherwise would send somebody to check a database that is
 * fine (AGENTS.md sections 28 and 45).
 */
export function headlinePreviewProblem(problem: LibraryProblem): string {
  return problem.code === 'unknown_command'
    ? 'This recorder cannot make thumbnails'
    : 'That thumbnail could not be read';
}

/** What to tell somebody about a thumbnail that never arrived. */
export function describePreviewProblem(problem: LibraryProblem): string {
  switch (problem.code) {
    case 'unknown_command':
      // Reachable even with the feature gate in front of it: a recorder can be
      // replaced by an older one while the window is open, and the answer to
      // the round trip already in flight arrives after that.
      return 'The recorder that is running is older than this window and has no way to make a thumbnail. Restarting Clipped starts the recorder that came with it.';
    case 'no_recorder_configured':
    case 'recorder_unreachable':
      return `Clipped could not ask the recorder for that thumbnail. ${problem.message}`;
    default:
      return problem.message;
  }
}

/**
 * Where a screen stands with one recording's peaks.
 *
 * An alias for {@link ThumbnailView} rather than a shape of its own, because
 * one command answers both and the four cases are the four cases: nothing was
 * asked, a round trip is in flight, the recorder answered in one of its three
 * states, or the asking itself failed. A second type would be a second thing to
 * keep in step for no difference (AGENTS.md section 55).
 */
export type WaveformView = ThumbnailView;

/**
 * Reads the peaks of `source`'s sound, at the width the caller can draw.
 *
 * `null` asks for nothing, for the reason {@link useThumbnail} takes it.
 *
 * # Why this does not go through the queue
 *
 * {@link CONCURRENT_PREVIEWS} exists for a *list*: a page of rows each opening
 * a pipe the moment it mounts. A waveform is asked for once, by one screen,
 * about the recording that screen is playing — one round trip, never twenty-
 * five — so queueing it would bound a thing that has no fan-out to bound, and
 * would put it behind whatever the queue happened to hold.
 *
 * It would also have to be keyed differently: the queue is keyed on the
 * recording alone, because two rows drawing the same file want the same
 * picture, and a waveform of the same file is a different answer to a different
 * question.
 *
 * # `buckets` is a decision, not a measurement
 *
 * Changing it asks again, which is why every caller so far passes a constant.
 * It must not be a measured width that moves with the window: merging buckets
 * is exact, so peaks answered at one width and drawn at another are the same
 * answer stretched rather than a wrong one, and re-asking the recorder on every
 * resize would be a round trip a frame for a picture nobody could tell from the
 * one already on the screen.
 */
export function useWaveform(source: string | null, buckets: number): WaveformView {
  const [answer, setAnswer] = useState<Answer | null>(null);

  useEffect(() => {
    if (source === null) {
      return;
    }
    let current = true;
    readPreview(source, 'waveform', buckets)
      .then((preview) => {
        if (current) {
          setAnswer({ source, result: { state: 'answered', preview } });
        }
      })
      .catch((thrown: unknown) => {
        if (current) {
          setAnswer({ source, result: { state: 'refused', problem: asProblem(thrown) } });
        }
      });
    return (): void => {
      current = false;
    };
  }, [source, buckets]);

  if (source === null) {
    return { state: 'unasked' };
  }
  const current = answer !== null && answer.source === source ? answer : null;
  return current === null ? { state: 'asking' } : current.result;
}
