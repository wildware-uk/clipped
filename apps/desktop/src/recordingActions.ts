import type { ExportSummary, LibraryRecording } from '@clipped/shared';
import { invoke } from '@tauri-apps/api/core';
import { save } from '@tauri-apps/plugin-dialog';
import { useCallback, useState } from 'react';

import { asProblem, formatBytes, type LibraryProblem } from './library';

/**
 * Doing something with a recording the library listed (issue #399).
 *
 * The library could say what you had recorded and nothing could be done with
 * it. These are the three things that close that: watch it, find it, share it.
 *
 * # Where each one actually happens
 *
 * Nowhere in this file, which is the point. The window has no file-system
 * permission and may not link the muxer
 * ([ADR 0002](../../../docs/adr/0002-separate-recorder-process.md)), so:
 *
 * - **Watching** and **finding** are Tauri commands. The Rust side asks Windows
 *   to open the file with whatever the user opens video with, or to show it in
 *   Explorer selected. They are commands rather than a Tauri permission because
 *   the permission that would let the interface do it directly is "open any
 *   path with its default application", and a recording lives wherever the
 *   recorder's output directory points (`src-tauri/src/main.rs`).
 * - **Sharing** is a round trip to the recorder, which copies the coded packets
 *   into an MP4 without re-encoding them (`docs/muxing.md`). The destination is
 *   the one thing chosen here, through the operating system's own Save As
 *   dialog — which is what `dialog:allow-save` in `capabilities/default.json`
 *   is for, and the whole of what this ticket added to the window's privilege.
 *
 * # Why a recording that has gone is not offered
 *
 * A recording whose file the library could not find carries `missing_since`,
 * and none of the three can be done to one. The controls are disabled and say
 * why rather than being drawn and failing, which is AGENTS.md section 27 —
 * `canActOn` is that rule in one place, so the three controls cannot disagree.
 */

/** Whether anything can be done with this recording. */
export function canActOn(recording: LibraryRecording): boolean {
  return recording.missing_since === undefined;
}

/**
 * The MP4 name offered when the Save As dialog opens.
 *
 * The recording's own name with `.mp4` in place of its extension, in the
 * recording's own folder, because that name already says which game and when
 * (`docs/sessions.md`) and that folder is where somebody looking for it will
 * look. It is a **suggestion**: the dialog is where it is accepted or changed.
 */
export function suggestedDestination(path: string): string {
  const separator = Math.max(path.lastIndexOf('\\'), path.lastIndexOf('/'));
  const directory = path.slice(0, separator + 1);
  const name = path.slice(separator + 1);
  const dot = name.lastIndexOf('.');
  const stem = dot > 0 ? name.slice(0, dot) : name;
  return `${directory}${stem}.mp4`;
}

/** Opens a recording in whatever application the user opens video with. */
export async function openRecording(path: string): Promise<void> {
  return invoke<void>('open_recording', { path });
}

/** Shows a recording in Explorer, with the file selected. */
export async function revealRecording(path: string): Promise<void> {
  return invoke<void>('reveal_recording', { path });
}

/** Copies a recording into MP4, and waits for the file. */
export async function exportRecording(source: string, destination: string): Promise<ExportSummary> {
  return invoke<ExportSummary>('export_recording', { source, destination });
}

/**
 * Asks the operating system where an MP4 should go.
 *
 * `null` when the dialog was dismissed, which is not a failure and must not be
 * reported as one.
 */
export async function chooseDestination(source: string): Promise<string | null> {
  return save({
    title: 'Export recording as MP4',
    defaultPath: suggestedDestination(source),
    filters: [{ name: 'MP4 video', extensions: ['mp4'] }],
  });
}

/** What the last thing somebody asked for turned out to be. */
export type ActionOutcome =
  | { readonly state: 'idle' }
  | { readonly state: 'working'; readonly path: string; readonly what: string }
  | { readonly state: 'done'; readonly message: string }
  | { readonly state: 'failed'; readonly problem: LibraryProblem };

/** The three things a library row offers, and what the last one produced. */
export interface RecordingActions {
  /** The outcome to put on screen, and whether something is in flight. */
  readonly outcome: ActionOutcome;
  /** Opens the recording in the system player. */
  readonly open: (recording: LibraryRecording) => void;
  /** Shows the recording in Explorer, selected. */
  readonly reveal: (recording: LibraryRecording) => void;
  /** Asks where the MP4 should go, and then writes it. */
  readonly exportToMp4: (recording: LibraryRecording) => void;
}

/**
 * The three actions, and the one outcome the screen reports.
 *
 * One outcome rather than one per row: only one of these can be in flight at a
 * time — the export is a round trip on a control connection and the other two
 * are a shell call — and a screen that kept a message against every row would
 * accumulate stale ones nobody dismissed.
 */
export function useRecordingActions(): RecordingActions {
  const [outcome, setOutcome] = useState<ActionOutcome>({ state: 'idle' });

  /** Runs one action, reporting what it did or why it did not. */
  const run = useCallback(
    (recording: LibraryRecording, what: string, act: () => Promise<string | null>): void => {
      setOutcome({ state: 'working', path: recording.path, what });
      act()
        .then((message) => {
          // `null` is "the person changed their mind", which is not an outcome
          // to report: a screen saying "cancelled" after somebody pressed
          // Escape is noise (AGENTS.md section 28).
          setOutcome(message === null ? { state: 'idle' } : { state: 'done', message });
        })
        .catch((thrown: unknown) => {
          setOutcome({ state: 'failed', problem: asProblem(thrown) });
        });
    },
    [],
  );

  const open = useCallback(
    (recording: LibraryRecording): void => {
      run(recording, 'Opening', async () => {
        await openRecording(recording.path);
        return `Opened ${fileName(recording.path)}.`;
      });
    },
    [run],
  );

  const reveal = useCallback(
    (recording: LibraryRecording): void => {
      run(recording, 'Showing', async () => {
        await revealRecording(recording.path);
        return `Showed ${fileName(recording.path)} in Explorer.`;
      });
    },
    [run],
  );

  const exportToMp4 = useCallback(
    (recording: LibraryRecording): void => {
      run(recording, 'Exporting', async () => {
        const destination = await chooseDestination(recording.path);
        if (destination === null) {
          return null;
        }
        const summary = await exportRecording(recording.path, destination);
        return describeExport(summary);
      });
    },
    [run],
  );

  return { outcome, open, reveal, exportToMp4 };
}

/** The file at the end of a path, for a sentence about it. */
export function fileName(path: string): string {
  const separator = Math.max(path.lastIndexOf('\\'), path.lastIndexOf('/'));
  return path.slice(separator + 1);
}

/**
 * What an export turned out to be, in one sentence.
 *
 * The measurement is included because it is the whole argument for a remux over
 * a re-encode, and because a copy that finished in four seconds looks like one
 * that did not happen (AGENTS.md section 18). A copy that could not hold
 * everything says so: `lossless` is false only for things beside the recording
 * — chapter marks, an attached font — because a picture or a sound track MP4
 * cannot carry is a refusal rather than a quiet loss.
 */
export function describeExport(summary: ExportSummary): string {
  const size = formatBytes(summary.bytes);
  const seconds = (summary.elapsed_ms / 1000).toFixed(1);
  const written = `Exported ${fileName(summary.destination)} — ${size} copied in ${seconds} s, without re-encoding.`;
  const losses = summary.losses ?? [];
  return summary.lossless || losses.length === 0
    ? written
    : `${written} Not carried into MP4: ${losses.join('; ')}.`;
}

/**
 * What the window tells somebody about an action that did not happen.
 *
 * Every branch here is a different useful action (AGENTS.md section 45), and
 * the default is the message the other side sent rather than a sentence of this
 * window's own — which for an export is the muxer's own account of what MP4
 * could not hold, and is the whole reason it crosses the boundary.
 */
export function describeActionProblem(problem: LibraryProblem): string {
  switch (problem.code) {
    case 'destination_exists':
      return `${problem.message} Clipped never writes over a file, so nothing was changed.`;
    case 'file_missing':
      return `${problem.message} The library will notice next time it checks.`;
    case 'unknown_command':
      return 'The recorder that is running is older than this window and cannot export a recording. Restarting Clipped starts the recorder that came with it.';
    case 'no_recorder_configured':
    case 'recorder_unreachable':
      return `Clipped could not ask the recorder to export that recording. ${problem.message}`;
    default:
      return problem.message;
  }
}

/**
 * The heading that goes above {@link describeActionProblem}.
 *
 * A destination that is taken is not a failure of the export, and saying it is
 * would send somebody looking for a fault that is not there: the recording is
 * fine, the muxer is fine, and the only thing to do is pick another name
 * (AGENTS.md sections 28 and 45).
 */
export function headlineActionProblem(problem: LibraryProblem): string {
  return problem.code === 'destination_exists'
    ? 'That name is already taken'
    : 'That could not be done';
}
