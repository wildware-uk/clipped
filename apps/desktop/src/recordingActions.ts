import type { ExportProgress, ExportSummary, LibraryRecording } from '@clipped/shared';
import { exportFraction } from '@clipped/shared';
import { invoke } from '@tauri-apps/api/core';
import { listen } from '@tauri-apps/api/event';
import { save } from '@tauri-apps/plugin-dialog';
import { useCallback, useEffect, useState } from 'react';

import { asProblem, formatBytes, type LibraryProblem } from './library';
import { inTauriWindow, recorderCanDo, type RecorderLinkState } from './useRecorderLink';

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
export function canActOn(recording: OnDisk): boolean {
  return recording.missing_since === undefined;
}

/**
 * Something of the user's on disk that a row can act on.
 *
 * A recording and a clip cut from one differ in almost everything and in none
 * of what opening or revealing a file needs: a path, and whether the library
 * has noticed it is gone. Typing these two actions against the pair of fields
 * they actually read is what lets the Library show a saved replay beside the
 * recording it came from — which is MVP step 12, *see the replay*, and was
 * unreachable while every control insisted on a `LibraryRecording`.
 *
 * Export is deliberately not here. It resolves a destination and asks the
 * recorder to transcode, and whether that is the right thing to offer for a
 * clip is a question rather than a widening.
 */
export interface OnDisk {
  /**
   * Where the file is.
   *
   * Required, unlike a clip's own `path`, which the protocol makes optional
   * because the library can hold a clip it has never seen a file for. There is
   * nothing to open or reveal in that case, so a caller with an optional path
   * checks it before offering a control rather than offering one that fails.
   */
  readonly path: string;
  /** When the library first found the file gone. */
  readonly missing_since?: string;
}

/**
 * Why there is no file, for a recording nothing can be done with.
 *
 * Three states and not one, because "there is no file" has three causes and a
 * user can act on only one of them
 * ([issue #673](https://github.com/wildware-uk/clipped/issues/673)):
 *
 * - `file-gone` — it recorded, and the file has since been moved or deleted.
 *   Something was lost.
 * - `never-recorded` — the recording failed before anything was written.
 *   Nothing was lost, and the sitting's own file says what went wrong.
 * - `no-window` — Clipped never found a window to record.
 *
 * Saying `file-gone` for the other two tells somebody they lost footage they
 * never had, which is the mistake `LibraryScreen`'s `putBack` already refuses
 * to make about a clip nothing has exported (issue #593).
 */
export type AbsentBecause = 'file-gone' | 'never-recorded' | 'no-window';

/**
 * Which of the three it is, or [`undefined`] when the file is there.
 *
 * Keyed on `missing_since` first, so this cannot disagree with
 * {@link canActOn}: a recording whose file is present is available whatever its
 * outcome says, and `outcome` only chooses the wording for one that is not.
 */
export function absentBecause(recording: LibraryRecording): AbsentBecause | undefined {
  if (canActOn(recording)) {
    return undefined;
  }
  if (recording.outcome === 'failed') {
    return 'never-recorded';
  }
  if (recording.outcome === 'no-window') {
    return 'no-window';
  }
  return 'file-gone';
}

/** The few words a row shows beside the file name. */
export function absenceLabel(because: AbsentBecause): string {
  switch (because) {
    case 'never-recorded':
      return 'did not record';
    case 'no-window':
      return 'no window found';
    case 'file-gone':
      return 'file missing';
  }
}

/** The sentence a disabled control gives as its reason. */
export function absenceReason(because: AbsentBecause): string {
  switch (because) {
    case 'never-recorded':
      return 'This recording never started, so there is no file. The sitting\u2019s own file, beside where the recording would have been, says what went wrong.';
    case 'no-window':
      return 'Clipped never found a window to record, so there is no file.';
    case 'file-gone':
      return 'This file could not be found the last time Clipped looked for it.';
  }
}

/**
 * What a window says about an export a recorder built before #399 cannot do.
 *
 * One sentence, said in two places: here, *before* the Export control is drawn
 * ({@link exportOffer}), and in {@link describeActionProblem}, *after* an
 * `export_recording` was refused with `unknown_command`. The second is the
 * older path and stays, because a recorder can be replaced by an older one
 * between the moment the control was drawn and the moment it was pressed — the
 * check is what stops that being the ordinary case rather than a race.
 *
 * Shared rather than typed twice so the two cannot come to disagree about what
 * a user should do (AGENTS.md section 55).
 */
const RECORDER_CANNOT_EXPORT =
  'The recorder that is running is older than this window and cannot export a recording. ' +
  'Restarting Clipped starts the recorder that came with it.';

/**
 * Whether an Export control may be drawn, and what it says when it may not.
 *
 * A control that is offered and then refused has told the user something untrue
 * (AGENTS.md section 27), and for an export the refusal is the expensive kind:
 * it arrives after the Save As dialog, so the person has already chosen a name
 * for a file that was never going to be written
 * ([issue #447](https://github.com/wildware-uk/clipped/issues/447)).
 *
 * Not offered is never silent. `shortly` goes in the control's own label, the
 * way the tray's menu items carry their reason after a dash
 * (`src-tauri/src/tray_model.rs`), and `why` is the whole sentence for a title
 * and for a screen reader.
 */
export type ExportOffer =
  | { readonly offered: true }
  | {
      readonly offered: false;
      /** A few words for the control's own label, after a dash. */
      readonly shortly: string;
      /** The whole sentence, for a title and for a screen reader. */
      readonly why: string;
    };

/**
 * Whether the recorder this window is attached to can export, from its welcome.
 *
 * # Four answers, not two
 *
 * The interesting one is `connecting`, which issue #447 asks about by name.
 * Features are not known until a recorder answers, so "this recorder cannot
 * export" would be a claim about a recorder nobody has spoken to yet — and one
 * that would turn into a working control a moment later. The control waits and
 * says it is waiting, which is the honest reading of a state whose whole
 * meaning is "not known yet" and does not flicker between two claims.
 *
 * `reconnecting` and `unavailable` are not that: there is no recorder, which is
 * a fact rather than an absence of one, and the link's own reason for it is
 * more use than anything this file could say.
 *
 * A `null` link is this page not being the Clipped window at all (`npm run
 * dev:web`), which `describeRecorderLink` already renders in the sidebar and is
 * repeated here because a disabled control with no reason beside it is the
 * thing section 27 rules out.
 */
export function exportOffer(link: RecorderLinkState | null): ExportOffer {
  if (link === null) {
    return {
      offered: false,
      shortly: 'not the Clipped window',
      why: 'This page is not the Clipped window, so there is no recorder to export with. Run npm run dev.',
    };
  }

  switch (link.link) {
    case 'connecting':
      return {
        offered: false,
        shortly: 'waiting for the recorder',
        why: 'Clipped is still looking for the recorder, and a recorder says what it can do when it answers.',
      };
    case 'reconnecting':
    case 'unavailable':
      return {
        offered: false,
        shortly: 'no recorder',
        why: `There is no recorder to export with. ${link.reason}`,
      };
    case 'attached':
      return recorderCanDo(link, 'export')
        ? { offered: true }
        : { offered: false, shortly: 'this recorder cannot export', why: RECORDER_CANNOT_EXPORT };
  }
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
  | {
      readonly state: 'working';
      readonly path: string;
      readonly what: string;
      /**
       * How far a running export has got, once the recorder has said.
       *
       * `null` until the first `export_progress` event arrives, and `null` for
       * ever against a recorder that does not have the `export_progress`
       * feature — which is a recorder that copies the file exactly as it always
       * did and says nothing while it does (issue #446). A screen draws an
       * unbounded indication for `null` rather than a bar at nought, because a
       * bar that never moves is a control that does nothing (AGENTS.md
       * section 27).
       *
       * Also `null` for the two actions that are not exports: opening a file
       * and showing it in Explorer are a shell call each and finish before
       * there is anything to say about them.
       */
      readonly progress: ExportProgress | null;
    }
  | { readonly state: 'done'; readonly message: string }
  | { readonly state: 'failed'; readonly problem: LibraryProblem };

/**
 * The name the Rust side emits recorder link events under.
 *
 * The same event `useRecorderLink` follows. Two subscriptions to one Tauri
 * event rather than threading the link through the screen: they want different
 * halves of it, and a subscription is a callback in a list.
 */
const LINK_EVENT = 'recorder-link';

/** The one link event this file cares about. */
interface ExportProgressEnvelope {
  readonly event: 'export_progress';
  readonly [key: string]: unknown;
}

/**
 * How far through an export, in the words a status line uses.
 *
 * A percentage where the recording said how long it was, and the bytes copied
 * where it did not — an interrupted recording keeps every packet it wrote and
 * no total, and "0 %" would be a worse answer than none.
 */
export function describeExportProgress(progress: ExportProgress): string {
  const fraction = exportFraction(progress);
  return fraction === null
    ? `${formatBytes(progress.bytes)} copied so far`
    : `${Math.round(fraction * 100)}%`;
}

/** The three things a library row offers, and what the last one produced. */
export interface RecordingActions {
  /** The outcome to put on screen, and whether something is in flight. */
  readonly outcome: ActionOutcome;
  /**
   * Whether an Export control may be drawn at all, and what it says otherwise.
   *
   * A property of the *recorder* rather than of a recording, so it is asked
   * once here rather than per row: every row on the screen is talking to the
   * same recorder, and forty rows disagreeing about whether it can export
   * would be forty chances to get it wrong (issue #447).
   */
  readonly canExport: ExportOffer;
  /** Opens the recording in the system player. */
  readonly open: (item: OnDisk) => void;
  /** Shows the recording in Explorer, selected. */
  readonly reveal: (item: OnDisk) => void;
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
/** How the outcome is set, including from inside a subscription. */
type SetOutcome = (next: (outcome: ActionOutcome) => ActionOutcome) => void;

export function useRecordingActions(link: RecorderLinkState | null): RecordingActions {
  const [outcome, setOutcome] = useState<ActionOutcome>({ state: 'idle' });

  useExportProgress(setOutcome);

  /** Runs one action, reporting what it did or why it did not. */
  const run = useCallback(
    (recording: OnDisk, what: string, act: () => Promise<string | null>): void => {
      setOutcome({ state: 'working', path: recording.path, what, progress: null });
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
    (recording: OnDisk): void => {
      run(recording, 'Opening', async () => {
        await openRecording(recording.path);
        return `Opened ${fileName(recording.path)}.`;
      });
    },
    [run],
  );

  const reveal = useCallback(
    (recording: OnDisk): void => {
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

  return { outcome, canExport: exportOffer(link), open, reveal, exportToMp4 };
}

/**
 * Folds `export_progress` events into whichever export is in flight.
 *
 * Matched on the **source**, because that is what this screen knows: a row's
 * `path` is the recording, and the destination is inside the Save As dialog's
 * answer, which nothing here kept. An event for a different recording is
 * ignored rather than shown, which is what stops a second window's export
 * moving this one's bar.
 *
 * Guarded on `state === 'working'` so that a late event — one already in the
 * queue when the reply landed — cannot reopen a finished outcome and replace
 * the sentence saying the file was written.
 */
function useExportProgress(setOutcome: SetOutcome): void {
  useEffect(() => {
    if (!inTauriWindow()) {
      return;
    }

    let current = true;
    const subscription = listen<ExportProgressEnvelope>(LINK_EVENT, ({ payload }) => {
      if (!current || payload.event !== 'export_progress') {
        return;
      }
      const progress = payload as unknown as ExportProgress;
      setOutcome((outcome) =>
        outcome.state === 'working' && outcome.path === progress.source
          ? { ...outcome, progress }
          : outcome,
      );
    });

    return () => {
      current = false;
      subscription
        // Wrapped rather than called bare: unsubscribing is a round trip to the
        // Rust side and returns a promise, and a bare call leaves its rejection
        // unhandled — which in a webview is a console error nobody reads.
        .then((unlisten) => Promise.resolve<void>(unlisten()))
        .catch(() => {
          // Nothing to do: the listener is going away with the screen.
        });
    };
  }, [setOutcome]);
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
      // Still reachable, and deliberately so: the recorder can be replaced
      // between the moment `exportOffer` drew the control and the moment it was
      // pressed. The check turns this from the ordinary path into a race.
      return RECORDER_CANNOT_EXPORT;
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
