import type { InterruptedRecording, RecorderLinkView } from './useRecorderLink';

/**
 * What the clip playback screen (issue #52) can say about a recording, and why
 * it cannot say much.
 *
 * Everything here is a pure function of what the window has been told, so that
 * the wording is testable without a window and every case has exactly one
 * rendering rather than a chain of conditions inside a component — the same
 * arrangement `gameDetection.ts` uses for the Games screen.
 *
 * # Why there is no player
 *
 * A recording is Matroska with uncompressed audio, and this window has no way
 * to load a file from the disk at all. Four separate facts stand in the way,
 * each of them enough on its own; they are in {@link PLAYBACK_BLOCKERS}, with
 * the evidence for each, and `docs/desktop-ui.md` ("Playing a recording") has
 * the design that follows from them. Issue #304 builds it.
 *
 * Drawing a transport with nothing behind it would be the control AGENTS.md
 * section 27 forbids, and drawing a poster frame Clipped never generated would
 * be the invented data of the same section.
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
          'The recorder is still writing this file. It can be played when the recording has ended and Clipped can serve it — issue #304.',
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

/** One reason a `<video>` in this window cannot play a Clipped recording. */
export interface Blocker {
  /** The fact, in a few words. */
  readonly fact: string;
  /** Where it can be checked, rather than taken on trust. */
  readonly evidence: string;
}

/**
 * Everything that stands between this window and a playing recording.
 *
 * Each of the four is enough on its own, which is why they are a list and not a
 * chain: fixing any one of them changes nothing. The fourth is the one that
 * decides the design, because it rules out every arrangement that hands a whole
 * file to a media element, whatever the container is.
 *
 * `playbackReach.test.ts` reads the Tauri configuration and asserts the first,
 * so the day somebody enables the asset protocol or widens the policy, that
 * test fails and brings them here rather than leaving this paragraph quietly
 * wrong.
 */
export const PLAYBACK_BLOCKERS: readonly Blocker[] = [
  {
    fact: 'This window cannot load a file from the disk',
    evidence:
      "src-tauri/tauri.conf.json does not enable the asset protocol, capabilities/default.json grants three core: permissions and none reaches the file system, and the content-security policy declares no media-src, so it falls back to default-src 'self' — the bundle Vite built, and nothing else.",
  },
  {
    fact: 'A recording is Matroska, and WebView2 does not demux it',
    evidence:
      'ADR 0001 writes recordings into MKV so that a killed recorder still leaves a playable file. WebView2 is Chromium, whose Matroska support is WebM: a strict subset restricted to Opus or Vorbis audio and VP8, VP9 or AV1 video.',
  },
  {
    fact: 'The audio is uncompressed PCM, and nothing in Clipped encodes audio',
    evidence:
      'docs/muxing.md: every track is 16-bit PCM because no crate in the workspace encodes audio (issue #28). No browser decodes PCM in MP4, so issue #92’s remux — which copies streams rather than re-encoding them — would carry the video across and leave the sound behind.',
  },
  {
    fact: 'A media element cannot choose an audio track',
    evidence:
      'HTMLMediaElement.audioTracks is not implemented in Chromium, so a multi-track file gives whichever track the demuxer lands on and no way off it. Issue #52 asks for a track selector, so the track has to be chosen on the way out of the recorder rather than in the element.',
  },
];

/** One thing this screen will show, and the work that has to land before it can. */
export interface Missing {
  /** What the playback screen will do. */
  readonly shows: string;
  /** What has to exist before it can, ending in the issue that builds it. */
  readonly needs: string;
}

/**
 * Everything issue #52 asks of this screen, against what each part waits for.
 *
 * The alternative was a transport bar, a scrubber and a track selector drawn
 * over a black rectangle. That is the interface AGENTS.md section 27 forbids
 * twice over — controls that do nothing, above a picture Clipped never made.
 */
export const MISSING: readonly Missing[] = [
  {
    shows: 'Play a recording at all, with sound',
    needs:
      'The recorder serving its media: a stream remuxed to fragmented MP4, with the audio encoded, answering byte ranges. Issue #304',
  },
  {
    shows: 'Play the compatibility mix by default, and let another track be chosen',
    needs:
      'The same stream, taking a track — because a media element cannot choose one. The mix already leads the file and carries Matroska’s default flag (docs/muxing.md). Issue #304',
  },
  {
    shows:
      'Open a recording somebody picked, rather than one this window happened to be told about',
    needs:
      'The library index reaches the window since issue #301, and the Library screen lists it. What is left is looking one recording up by the identifier in the address bar. Issue #52',
  },
  {
    shows: 'Say that a recording’s file has gone, rather than drawing a player that does nothing',
    needs:
      'missing_since is on the wire since issue #301 and the Library screen says it. This screen looks up no recording to say it about. Issue #52',
  },
  {
    shows: 'A poster frame before playback starts',
    needs:
      'Thumbnails are generated, cached and tested (issue #57) and nothing has ever drawn one: the cache is a file beside the recording, and this window has no file-system permission. Issue #301 carried rows, not bytes',
  },
  {
    shows: 'A waveform under the transport, and bookmarks and events on it',
    needs:
      'Waveforms are issue #66 and are files too, so they need the same transport a thumbnail does; bookmarks are written beside the recording already (issue #64) and issue #65 draws a timeline.',
  },
];
