import type { PlaybackTrack } from '@clipped/shared';
import { invoke } from '@tauri-apps/api/core';
import { useEffect, useState } from 'react';

import { asProblem, type LibraryProblem } from './library';

/**
 * Playing a recording in this window (issue #304).
 *
 * # Where the media comes from
 *
 * Not from the disk: this window has no file-system permission and no asset
 * protocol, and `playbackReach.test.ts` holds that. It asks the Tauri host,
 * which asks the recorder, which opens the recording and answers with a file —
 * and the host registers that file with its `clip` scheme and hands back an
 * address. So what arrives here is `http://clip.localhost/3`, and a number
 * nothing has registered is a 404 (`src-tauri/src/playback.rs`).
 *
 * # Why choosing a track is a round trip and not a control on the element
 *
 * `HTMLMediaElement.audioTracks` is not implemented in Chromium, which WebView2
 * is. A `<video>` handed a recording with four sound tracks plays the first and
 * offers no way off it, so hearing the microphone on its own means being handed
 * a file that holds the microphone — which only the recorder can make. Choosing
 * a track is therefore `open_playback` again, with the track named, and a new
 * address to point the element at.
 *
 * # What is deliberately not measured here
 *
 * The duration, the picture size and the position. All three are the element's,
 * read from the media it was given. Nothing in this window computes a duration
 * or counts a clock (AGENTS.md section 27) — the same rule the record control
 * keeps.
 */

/** A recording the recorder has opened, as the Tauri host answers. */
export interface OpenedPlayback {
  /** Where to point a `<video>`. */
  readonly url: string;
  /** The source stream index whose sound is in it, if it has any. */
  readonly audio_track?: number;
  /** Every sound track of the recording, for the selector. */
  readonly audio_tracks: readonly PlaybackTrack[];
  /** Whether a copy had to be made to carry the chosen track. */
  readonly prepared: boolean;
}

/** Opens a recording for playback, on a track or on the recorder's choice. */
export async function openPlayback(source: string, audioTrack?: number): Promise<OpenedPlayback> {
  return invoke<OpenedPlayback>('open_playback', { source, audioTrack });
}

/** What a track is called on the selector. */
export function trackLabel(track: PlaybackTrack, position: number): string {
  // A recording that named none is shown by its position rather than given a
  // name here: "Audio 2" is a fact about the file, and "Game" would be a guess
  // about what is on it (AGENTS.md section 27).
  return track.name ?? `Audio ${String(position + 1)}`;
}

/** Where a screen stands with one recording. */
export interface PlaybackView {
  /** What to play, once the recorder has answered. */
  readonly stream: OpenedPlayback | null;
  /** Whether an open is in flight. */
  readonly busy: boolean;
  /** Why there is nothing to play, when there is a reason. */
  readonly problem: LibraryProblem | null;
  /** Asks for another of the recording's sound tracks. */
  readonly choose: (audioTrack: number) => void;
}

/** What the recorder said about one recording, on one track. */
interface Answer {
  /** The recording it was asked about. */
  readonly source: string;
  /** The track it was asked for, `undefined` for the recorder's own choice. */
  readonly track: number | undefined;
  /** What to play, when it could be opened. */
  readonly stream: OpenedPlayback | null;
  /** Why it could not be, when it could not. */
  readonly problem: LibraryProblem | null;
}

/**
 * Opens `source` for playback, and follows the track somebody chooses.
 *
 * `null` opens nothing, which is what a screen with no recording to play passes
 * — a hook cannot be called conditionally, and a screen that called this only
 * sometimes would be the bug that rule exists to prevent.
 *
 * # Why the state is one answer rather than three flags
 *
 * The answer carries **what it was an answer to**, and everything the screen
 * reads is derived from comparing that against what is being asked for now.
 * That is what makes two things true without a flag to keep in step: an answer
 * about the previous track is not shown as the current one, and the previous
 * stream stays on screen while the next is being prepared — so choosing a track
 * leaves the picture up instead of blanking it for as long as the recorder
 * takes. Nothing here sets state inside an effect; the effect asks, and the
 * reply sets the answer.
 */
export function usePlayback(source: string | null): PlaybackView {
  const [asked, setAsked] = useState<number | undefined>(undefined);
  const [answer, setAnswer] = useState<Answer | null>(null);

  useEffect(() => {
    if (source === null) {
      return;
    }
    let current = true;
    openPlayback(source, asked)
      .then((stream) => {
        if (current) {
          setAnswer({ source, track: asked, stream, problem: null });
        }
      })
      .catch((thrown: unknown) => {
        if (current) {
          // The recorder's own sentence — "match.mkv is not there any more…" —
          // rather than one invented here. It is the whole reason a refusal
          // crosses the boundary (AGENTS.md section 15).
          setAnswer({ source, track: asked, stream: null, problem: asProblem(thrown) });
        }
      });
    return () => {
      current = false;
    };
  }, [source, asked]);

  /** The answer to what is being asked for now, if it has arrived. */
  const current =
    answer !== null && answer.source === source && answer.track === asked ? answer : null;
  /** The last answer about this recording, whichever track it was about. */
  const ofThisRecording = answer !== null && answer.source === source ? answer : null;

  return {
    stream: current?.stream ?? ofThisRecording?.stream ?? null,
    // Only what is being asked for now can be refused: a refusal about the
    // track somebody has just moved off is not a reason to stop playing.
    problem: current?.problem ?? null,
    busy: source !== null && current === null,
    choose: setAsked,
  };
}

/**
 * The heading above a refusal, and then what to do about it.
 *
 * Separate from the Library's {@link describeActionProblem} because the useful
 * actions are different ones: nothing here has a destination to rename, and the
 * commonest failure by far — the file has gone — is one the recorder has
 * already worded.
 */
export function headlinePlaybackProblem(problem: LibraryProblem): string {
  return problem.code === 'unknown_command'
    ? 'This recorder cannot play a recording'
    : 'That recording cannot be played';
}

/** What to tell somebody about a recording that will not play. */
export function describePlaybackProblem(problem: LibraryProblem): string {
  switch (problem.code) {
    case 'unknown_command':
      return 'The recorder that is running is older than this window and has no way to open a recording for playback. Restarting Clipped starts the recorder that came with it; Open plays the file in whatever you already use meanwhile.';
    case 'no_recorder_configured':
    case 'recorder_unreachable':
      return `Clipped could not ask the recorder for that recording. ${problem.message}`;
    default:
      return problem.message;
  }
}
