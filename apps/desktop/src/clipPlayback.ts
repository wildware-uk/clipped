import type { LibraryRecording } from '@clipped/shared';

import type { InterruptedRecording, RecorderLinkView } from './useRecorderLink';

/**
 * What the clip playback screen (issue #52) can say about a recording, and
 * which file it plays.
 *
 * Everything here is a pure function of what the window has been told, so that
 * the wording is testable without a window and every case has exactly one
 * rendering rather than a chain of conditions inside a component — the same
 * arrangement `gameDetection.ts` uses for the Games screen.
 *
 * # Where the player came from
 *
 * There was none until issue #304, and the reasons filled a table on this
 * screen. Three of the four turned out to be wrong: a WebView2 plays a Clipped
 * recording — Matroska, AV1 picture, uncompressed PCM sound — as it is, which
 * `docs/adr/0010-what-the-webview-plays.md` records with the measurement behind
 * it. What was true, and still is, is that this window cannot reach a file on
 * its own and that a media element cannot choose an audio track. So the
 * recorder opens the recording and says what to play (`playback.ts`), and the
 * Tauri host serves that one file over a scheme of its own
 * (`src-tauri/src/playback.rs`).
 *
 * What is still not drawn is in {@link MISSING}, and each row names the work
 * that supplies it. Nothing here is drawn over an answer nobody gave: the
 * duration, the position and the picture size are the media element's own
 * measurements of the file it was handed (AGENTS.md section 27).
 */

/** The route this screen is mounted at, with `:recordingId` for the recording. */
export const CLIP_ROUTE = '/clip/:recordingId';

/**
 * Where a recording's playback screen is.
 *
 * A recording identifier comes from the recorder and is opaque to this window,
 * so it is encoded rather than trusted to be path-safe.
 */
export function clipPath(recordingId: string): string {
  return `/clip/${encodeURIComponent(recordingId)}`;
}

/** Whether a path is a clip playback screen's. */
export function isClipPath(pathname: string): boolean {
  return pathname.startsWith('/clip/');
}

/**
 * A recording this window can name, and the four things it knows about one.
 *
 * These are the protocol's own `active_recording` fields (`docs/ipc.md`), which
 * is the *only* description of a recording that reaches this window. There is
 * no duration, because nothing has measured one — a recording in progress has
 * an elapsed time and a recording a killed recorder left may have no Matroska
 * trailer at all — and there is no thumbnail, because #57's cache is a *file*
 * beside the recording, and #301 gave this window rows rather than bytes.
 */
export interface KnownRecording {
  /** The recorder's identifier for it. */
  readonly recordingId: string;
  /** The file, in full: the one thing here anybody can act on. */
  readonly output: string;
  /** What was being recorded, as the user asked for it: ``process `cs2.exe` ``. */
  readonly target: string;
  /** Milliseconds the recorder had been recording when it last said so. */
  readonly elapsedMs: number;
}

/**
 * What this window could find out about the recording in the address bar.
 *
 * Three cases, and the third is the ordinary one. The window follows a single
 * recorder and learns of exactly two recordings from it — the one being written
 * now, and the one a recorder died in the middle of — so any other identifier
 * is one *this screen* has not looked up. That is a different thing from "no
 * such recording", and it is said differently.
 *
 * Since #301 the library can be read, and this screen is not yet the thing that
 * reads it: the identifier in the address bar is the recorder's `recording_id`
 * for a live recording, and the index's own integer key is a different
 * identifier entirely. Reconciling the two, and opening a recording somebody
 * picked, is [issue #52](https://github.com/wildware-uk/clipped/issues/52).
 */
export type ClipResolution =
  /** The recorder is writing this recording as the screen is read. */
  | { readonly found: 'in-progress'; readonly recording: KnownRecording }
  /** A recorder died writing this one, and named the file it left (ADR 0006). */
  | { readonly found: 'interrupted'; readonly recording: KnownRecording }
  /** This window has no way to look a recording up. */
  | { readonly found: 'unindexed'; readonly recordingId: string };

/** An interruption, as a recording. */
function fromInterruption(interrupted: InterruptedRecording): KnownRecording {
  return {
    recordingId: interrupted.recording_id,
    output: interrupted.output,
    target: interrupted.target,
    elapsedMs: interrupted.elapsed_ms,
  };
}

/**
 * What this window knows about one recording.
 *
 * The order matters. A recorder that is writing this recording *now* is the
 * newer fact: a recording can be interrupted, and a replacement recorder can be
 * asked to record the same target again, and the identifier that is live beats
 * the one that is history.
 */
export function resolveClip(recordingId: string, view: RecorderLinkView): ClipResolution {
  const { link, interrupted } = view;

  if (link !== null && link.link === 'attached' && link.status.state === 'recording') {
    const status = link.status;
    if (status.recording_id === recordingId) {
      return {
        found: 'in-progress',
        recording: {
          recordingId: status.recording_id,
          output: status.output,
          target: status.target,
          elapsedMs: status.elapsed_ms,
        },
      };
    }
  }

  if (interrupted !== null && interrupted.recording_id === recordingId) {
    return { found: 'interrupted', recording: fromInterruption(interrupted) };
  }

  return { found: 'unindexed', recordingId };
}

/** A heading and one sentence, as the screen's panel draws them. */
export interface ClipDescription {
  /** The state, in two or three words. */
  readonly state: string;
  /** What that means for the person reading it. */
  readonly detail: string;
}

/**
 * What to say about a resolution.
 *
 * None of the three says a recording is missing, and that is deliberate: this
 * window cannot look at the disk, so "the file has gone" is a claim it has no
 * standing to make. `missing_since` in the library index is where that answer
 * lives (#56), #301 put it on the wire, and #52 is what would look this
 * particular recording up in it. Reporting a file as missing because *this*
 * screen could not find it would be exactly the invented state AGENTS.md
 * section 27 is about.
 */
export function describeClip(resolution: ClipResolution): ClipDescription {
  switch (resolution.found) {
    case 'in-progress':
      return {
        state: 'Being recorded now',
        detail:
          'The recorder is still writing this file. It can be played as soon as the recording has ended.',
      };
    case 'interrupted':
      return {
        state: 'Interrupted',
        detail:
          'A recorder died while writing this recording and it was not resumed. The file it left is below.',
      };
    case 'unindexed':
      return {
        state: 'Not known to this window',
        detail:
          'This window follows one recorder and learns of the recording it is writing, and of one a recorder died in the middle of. Everything else is in the library index, which the Library screen reads and this screen does not yet — issue #52. This is not a statement that the recording does not exist.',
      };
  }
}

/** The recording behind a resolution, where there is one. */
export function recordingOf(resolution: ClipResolution): KnownRecording | null {
  return resolution.found === 'unindexed' ? null : resolution.recording;
}

/**
 * How long the recorder said it had been recording.
 *
 * Not a duration: nothing has measured the file. It is the last elapsed time
 * the recorder reported, which for an interrupted recording is the last one the
 * link heard before the process went, so it is a lower bound on what is in the
 * file rather than the length of it. The wording says so.
 */
export function formatElapsed(milliseconds: number): string {
  const total = Math.max(0, Math.floor(milliseconds / 1000));
  const hours = Math.floor(total / 3600);
  const minutes = Math.floor((total % 3600) / 60);
  const seconds = total % 60;
  const pad = (value: number): string => String(value).padStart(2, '0');

  return hours === 0
    ? `${pad(minutes)}:${pad(seconds)}`
    : `${String(hours)}:${pad(minutes)}:${pad(seconds)}`;
}

/** A recording somebody opened this screen from, as the library listed it. */
export type HandedRecording = Pick<LibraryRecording, 'path' | 'missing_since'>;

/**
 * What the screen has to point a `<video>` at, or why it has nothing.
 *
 * Three sources, in this order, and the order is the point: a recording handed
 * over by the screen somebody came from is what they asked to watch, and the
 * link's own two recordings are what this window can name by itself.
 */
export type PlaybackSource =
  /** Play this file. */
  | { readonly file: string; readonly why: null }
  /** There is nothing to play, and this is what to say instead. */
  | { readonly file: null; readonly why: string };

/**
 * Which file this screen plays, given what it was told.
 *
 * A recording still being written is deliberately **not** played: its container
 * has no trailer yet, so a player would have no duration to seek in and would
 * be reading a file another process is appending to. Saying so beats a player
 * that behaves strangely for reasons nobody on screen can see.
 */
export function playbackSource(
  resolution: ClipResolution,
  handed: HandedRecording | null,
): PlaybackSource {
  if (handed !== null) {
    // The library already knows this file has gone, and it looked at the disk
    // to find that out. Saying so here costs no round trip and is the same
    // answer the recorder would give (`docs/library.md`, issue #56).
    return handed.missing_since === undefined
      ? { file: handed.path, why: null }
      : {
          file: null,
          why: 'The library could not find this file the last time it looked. It may have been moved or deleted, or the drive it is on may not be connected.',
        };
  }

  switch (resolution.found) {
    case 'in-progress':
      return {
        file: null,
        why: 'The recorder is still writing this file, so there is nothing finished to play. It can be played as soon as the recording has ended.',
      };
    case 'interrupted':
      return { file: resolution.recording.output, why: null };
    case 'unindexed':
      return {
        file: null,
        why: 'This window has not been told which file this recording is, so there is nothing to point a player at.',
      };
  }
}

/** One thing this screen will show, and the work that has to land before it can. */
export interface Missing {
  /** What the playback screen will do. */
  readonly shows: string;
  /** What has to exist before it can, ending in the issue that builds it. */
  readonly needs: string;
}

/**
 * What issue #52 asks of this screen and it does not yet do.
 *
 * Shorter than it was, because playing a recording and choosing its sound track
 * are on the screen now (issue #304). What is left is what would otherwise be
 * drawn over nothing: a poster frame Clipped has never produced on a real
 * machine, and a waveform that is a file this window has no route to.
 */
export const MISSING: readonly Missing[] = [
  {
    shows: 'A recording opened here directly, rather than from the screen you came from',
    needs:
      'The address carries the library’s own identifier and the row is handed over by whatever opened this screen, so a reload has nothing to look up. Looking one up cold is issue #52',
  },
  {
    shows: 'Frame-accurate seeking, and keyboard shortcuts of Clipped’s own',
    needs:
      'What is drawn is the media element’s own transport, which seeks to a keyframe. SPEC.md section 42 asks for more, and it is issue #52',
  },
  {
    shows: 'A poster frame before playback starts',
    needs:
      'Thumbnails are generated, cached and tested (issue #57) and nothing has ever drawn one: the cache is a file beside the recording, and the scheme this screen plays through serves recordings the recorder opened rather than pictures beside them',
  },
  {
    shows: 'A waveform under the transport, and bookmarks and events on it',
    needs:
      'Waveforms are issue #66 and are files too, so they need the same route a thumbnail does; bookmarks are written beside the recording already (issue #64) and issue #65 draws a timeline.',
  },
];
