import { PROTOCOL_VERSION } from '@clipped/shared';

import { name as INTERFACE_NAME, version as INTERFACE_VERSION } from '../package.json';
import { redactPath, redactPathsIn } from './redactPath';
import type { RecorderLinkView } from './useRecorderLink';

/**
 * What the Diagnostics screen says, and what the support report contains.
 *
 * Beside `DiagnosticsScreen.tsx` rather than inside it, for the reason
 * `gameDetection.ts` sits beside the Games screen: the wording and the report are
 * the parts worth testing, and they are testable without a window only if no
 * component is in the way.
 *
 * # What this window can establish, and what it cannot
 *
 * SPEC.md section 36 lists twelve things a recorder must record, and the window
 * can report exactly one of them — the path of a recording in progress, which
 * arrives inside a `recording` status. Everything else is inside the recorder
 * process with nothing to carry it here: `CaptureStatus` in `clipped-capture`
 * (#97), the capability report `clipped-recorder capabilities` prints (#14), and
 * the figures the `metrics` event stream would carry, which this recorder refuses
 * with `not_implemented` because nothing measures them yet (#100). Issue #302 is
 * the command that would bring them.
 *
 * So [`diagnostics`] below names all twelve, one row each, with what this build
 * reports against each — the same contract the Games screen keeps for its own
 * unbuilt table. Drawing twelve gauges reading zero would be indistinguishable
 * from a machine that dropped no frames, and this build has not counted
 * (AGENTS.md section 27).
 */

/** Where `clipped-logging` writes, in the form a user can paste into Explorer. */
export const LOG_DIRECTORY = String.raw`%LOCALAPPDATA%\Clipped\logs`;

/** The few words shown as the capture health state, and one sentence. */
export interface CaptureHealthText {
  /**
   * The state, in a few words.
   *
   * A phrase, not a grade. "Healthy" would be a claim about capture, and capture
   * is the one thing this window cannot see — so the states here are about the
   * recorder this window is attached to, which is what it can establish.
   */
  readonly state: string;
  /** One sentence saying what that means for the person reading it. */
  readonly detail: string;
  /**
   * What to do about it, when there is something.
   *
   * Absent when nothing is wrong. A "nothing to do" line under a working
   * recorder is noise, and AGENTS.md section 45 is about failures.
   */
  readonly action?: string;
}

/**
 * The scope of everything the health summary says, stated once.
 *
 * It is true in every state, so it is not repeated into the six renderings — the
 * same reasoning as `WHAT_WORKS_TODAY` on the Games screen. Two limits, and both
 * matter for reading the sentence above it: the window sees one recorder, the one
 * it started or attached to; and it has only been watching since it opened, so
 * "nothing has failed" means nothing has failed *since then*.
 */
export const SCOPE_OF_THIS_SUMMARY =
  'This describes the recorder this window is attached to, since this window opened. Capture ' +
  'itself — which backend is running, what it fell back from, how many frames were dropped — is ' +
  'not reported to this window by any build yet; issue #302 is the command that would carry it.';

/**
 * What to say about capture health, given everything the window knows.
 *
 * A pure function, so that every state has exactly one rendering rather than a
 * chain of conditions inside a component, and so the wording can be tested
 * without a window.
 *
 * **No rendering claims that nothing is being recorded** unless the recorder said
 * so. A link that dropped tells you the window lost sight of a recorder, not that
 * the recorder stopped: the recorder is a separate process precisely so that it
 * goes on recording when this one is not there
 * ([ADR 0002](../../../docs/adr/0002-separate-recorder-process.md)). The one
 * state that does say nothing is being recorded is `attached` and `idle`, where
 * the recorder itself is the source.
 */
export function describeCaptureHealth(view: RecorderLinkView): CaptureHealthText {
  const link = view.link;
  if (link === null) {
    return {
      state: 'Not known',
      detail:
        'This page is not the Clipped window, so there is no recorder to ask. Run npm run dev.',
    };
  }

  switch (link.link) {
    case 'connecting':
      return { state: 'Not known yet', detail: 'Looking for the recorder.' };
    case 'reconnecting':
      return {
        state: 'Not known',
        detail:
          `This window has lost sight of the recorder. ${link.reason} ` +
          `Attempt ${String(link.attempt)} of ${String(link.attempts_allowed)}. A recorder that ` +
          'is still running goes on recording either way; this window cannot see it to say.',
        action: 'Nothing yet. If the link gives up it says so here, and stops trying.',
      };
    case 'unavailable':
      return {
        state: 'No recorder',
        detail: link.reason,
        action:
          'Restarting Clipped makes the attempt again — a Try again control here is issue #221. ' +
          `The recorder's own account of what went wrong is in its log files, in ${LOG_DIRECTORY}.`,
      };
    case 'attached':
      return link.status.state === 'recording'
        ? {
            state: 'Recording',
            detail: `Recording ${link.status.target} to ${link.status.output}.`,
          }
        : {
            state: 'Ready',
            detail: 'The recorder is running and it says nothing is being recorded.',
          };
  }
}

/**
 * What has gone wrong since this window opened, worst thing first.
 *
 * Empty is the healthy case and is worth stating rather than leaving blank: a
 * screen that says nothing where a failure would go is indistinguishable from one
 * that is not watching. [`NOTHING_HAS_FAILED`] is what the screen draws instead.
 *
 * Both entries name a file, because that is the only part anybody can act on
 * (AGENTS.md section 45). A failed recording has one only when this window saw
 * the recording it names — `useRecorderLink` will not guess — and says so plainly
 * when it did not, rather than leaving the sentence to trail off.
 */
export function describeConcerns(view: RecorderLinkView): readonly string[] {
  const concerns: string[] = [];

  if (view.failed !== null) {
    const where =
      view.failed.output === null
        ? 'This window did not see which file it was writing, so it cannot name one.'
        : `The file it wrote up to the failure is at ${view.failed.output}.`;
    concerns.push(
      `A recording failed: ${view.failed.error.message} (${view.failed.error.code}) ${where}`,
    );
  }

  if (view.interrupted !== null) {
    concerns.push(
      'A recording was interrupted and not resumed: the recorder stopped while it was running. ' +
        `The file is at ${view.interrupted.output}.`,
    );
  }

  return concerns;
}

/** What the screen says where a failure would go, when there has been none. */
export const NOTHING_HAS_FAILED =
  'No recording has failed or been interrupted since this window opened.';

/** One of SPEC.md section 36's diagnostics, against what this build reports. */
export interface Diagnostic {
  /** The subject, in SPEC.md section 36's own words. */
  readonly subject: string;
  /** What this build reports about it, or what it is waiting on. */
  readonly reported: string;
  /** Whether `reported` is a value rather than the work that would supply one. */
  readonly known: boolean;
}

/** The sentence every unreported row ends with, so a reader knows where to look. */
const CARRIED_BY = 'Issue #302 is the command that would carry it.';

/**
 * Every diagnostic SPEC.md section 36 asks for, and where the log files are.
 *
 * The order is the specification's. One row is a value; the rest name the work
 * that would supply one, which is the alternative to twelve gauges reading zero.
 */
export function diagnostics(view: RecorderLinkView): readonly Diagnostic[] {
  const recording =
    view.link?.link === 'attached' && view.link.status.state === 'recording'
      ? view.link.status
      : null;

  return [
    {
      subject: 'Game detection',
      reported:
        'Not reported. The protocol describes a recording by its capture target — process ' +
        '4242 — and has no vocabulary for a game or a session. Issue #241.',
      known: false,
    },
    {
      subject: 'Capture backend',
      reported:
        'Not reported. clipped-capture already tracks the method in use, the method a recording ' +
        `started with, and every fallback and restart with its reason (issue #97). ${CARRIED_BY}`,
      known: false,
    },
    {
      subject: 'Resolution changes',
      reported: `Not reported. Issue #98 owns the behaviour. ${CARRIED_BY}`,
      known: false,
    },
    {
      subject: 'Encoder',
      reported:
        'Not reported. clipped-recorder capabilities prints the adapters, the encoder runtimes, ' +
        'the codecs the installed driver registers and the limits it measured (issue #14), and ' +
        `it prints them to a terminal. ${CARRIED_BY}`,
      known: false,
    },
    {
      subject: 'Dropped frames',
      reported:
        'Not reported. The metrics event stream is defined and this recorder refuses it with ' +
        'not_implemented, because nothing measures these during a recording yet. Issue #100.',
      known: false,
    },
    {
      subject: 'Encoder latency',
      reported: 'Not reported. Nothing measures it during a recording. Issue #100.',
      known: false,
    },
    {
      subject: 'Audio drift',
      reported: 'Not reported. Nothing measures it during a recording. Issue #100.',
      known: false,
    },
    {
      subject: 'Audio devices',
      reported: 'Not reported. No recording captures audio yet. Issue #180.',
      known: false,
    },
    {
      subject: 'Recording paths',
      reported:
        recording === null
          ? 'Nothing is being recorded, so there is no path. This is the one diagnostic on this ' +
            'list the window can report, and it arrives inside a recording status.'
          : recording.output,
      known: recording !== null,
    },
    {
      subject: 'Muxer status',
      reported: 'Not reported. Nothing says whether the muxer is keeping up. Issue #100.',
      known: false,
    },
    {
      subject: 'Disk latency',
      reported: 'Not reported. Nothing measures it. Issue #100.',
      known: false,
    },
    {
      subject: 'Plugin events',
      reported: 'Not reported. There is no plugin system. Issue #69.',
      known: false,
    },
    {
      subject: 'Log files',
      reported:
        `${LOG_DIRECTORY}, rotated hourly with the newest 48 kept. This window can neither read ` +
        'them nor open the folder: it is granted three Tauri permissions and none of them ' +
        'touches the file system. Issue #303.',
      known: false,
    },
  ];
}

/** Everything the support report is composed from. */
export interface DiagnosticsReportInput {
  /** Everything the window knows about the recorder. */
  readonly view: RecorderLinkView;
  /** Whatever failed before the window existed, or the tray had to report. */
  readonly notice: string | undefined;
  /** `navigator.userAgent`: the Windows build and the WebView2 version. */
  readonly userAgent: string;
  /** When the report was composed. */
  readonly takenAt: Date;
}

/** A duration a person reads, from the milliseconds the protocol carries. */
function formatDuration(milliseconds: number): string {
  const totalSeconds = Math.floor(milliseconds / 1000);
  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;

  if (hours > 0) {
    return `${String(hours)} h ${String(minutes)} min ${String(seconds)} s`;
  }
  if (minutes > 0) {
    return `${String(minutes)} min ${String(seconds)} s`;
  }
  return `${String(seconds)} s`;
}

/** A label and its value, as one line of the report. */
type Field = readonly [label: string, value: string];

/** The widest label, so the values line up in a monospaced block. */
function renderFields(fields: readonly Field[]): readonly string[] {
  const width = Math.max(...fields.map(([label]) => label.length));
  return fields.map(([label, value]) => `${label.padEnd(width)}  ${value}`);
}

/** What the link is doing, in one word, for the report's first field. */
function linkState(view: RecorderLinkView): string {
  if (view.link === null) {
    return 'none — this interface is not running inside the Clipped window';
  }
  return view.link.link;
}

/** The fields describing the recorder, which is most of the report. */
function recorderFields(view: RecorderLinkView): readonly Field[] {
  const fields: Field[] = [['Recorder link', linkState(view)]];
  const link = view.link;

  if (link?.link === 'attached') {
    fields.push(['Recorder process', String(link.recorder_process_id)]);
    fields.push(['Recorder state', link.status.state]);
    if (link.status.state === 'recording') {
      fields.push(['Recording id', link.status.recording_id]);
      fields.push(['Recording target', redactPathsIn(link.status.target)]);
      fields.push(['Recording file', redactPath(link.status.output)]);
      fields.push(['Elapsed when observed', formatDuration(link.status.elapsed_ms)]);
    }
  }

  if (link?.link === 'reconnecting') {
    fields.push(['Attempt', `${String(link.attempt)} of ${String(link.attempts_allowed)}`]);
    fields.push(['Delay', `${String(link.delay_ms)} ms`]);
    fields.push(['Reason', redactPathsIn(link.reason)]);
  }

  if (link?.link === 'unavailable') {
    fields.push(['Reason', redactPathsIn(link.reason)]);
  }

  return fields;
}

/** The fields describing what has gone wrong, which is why anybody sends this. */
function failureFields(view: RecorderLinkView): readonly Field[] {
  const fields: Field[] = [];

  if (view.failed === null) {
    fields.push(['Recording failed', 'none since this window opened']);
  } else {
    fields.push(['Recording failed', view.failed.recording_id]);
    fields.push(['  seen', view.failed.seenAt.toISOString()]);
    fields.push(['  code', view.failed.error.code]);
    fields.push(['  message', redactPathsIn(view.failed.error.message)]);
    fields.push([
      '  file',
      view.failed.output === null ? 'not seen by this window' : redactPath(view.failed.output),
    ]);
  }

  if (view.interrupted === null) {
    fields.push(['Recording interrupted', 'none since this window opened']);
  } else {
    fields.push(['Recording interrupted', view.interrupted.recording_id]);
    fields.push(['  target', redactPathsIn(view.interrupted.target)]);
    fields.push(['  file', redactPath(view.interrupted.output)]);
    fields.push(['  elapsed', formatDuration(view.interrupted.elapsed_ms)]);
  }

  return fields;
}

/**
 * The support report: everything this window can establish, as plain text.
 *
 * # The two rules it is built to
 *
 * **Every free-text value goes through `redactPathsIn`, and every path through
 * `redactPath`.** A report is pasted into a bug tracker, which is further than a
 * log file on somebody's own disk ever travels, so it must not carry what
 * `docs/logging.md` was careful to keep out of the logs — and the recorder's own
 * sentences do carry paths: `the recorder was not found at
 * C:\Users\alice\...\clipped-recorder.exe` names an account. There is no field
 * here whose value is passed through untouched.
 *
 * **Nothing is invented.** A figure nothing measured is absent, and the report
 * ends by naming what this build does not report, so that a reader can tell
 * "dropped no frames" from "counted no frames" (AGENTS.md section 27). `Elapsed
 * when observed` carries the observation time beside it for the same reason: it
 * is measured when the recorder answers, and a status from four minutes ago would
 * otherwise read as current.
 */
export function buildDiagnosticsReport(input: DiagnosticsReportInput): string {
  const health = describeCaptureHealth(input.view);
  const unreported = diagnostics(input.view)
    .filter((entry) => !entry.known)
    .map((entry) => entry.subject);

  const fields: Field[] = [
    ['Taken', input.takenAt.toISOString()],
    ['Interface', `${INTERFACE_NAME} ${INTERFACE_VERSION}`],
    ['Protocol version', String(PROTOCOL_VERSION)],
    ['Webview', input.userAgent],
    [
      'Status observed',
      input.view.observedAt === null ? 'never' : input.view.observedAt.toISOString(),
    ],
    ...recorderFields(input.view),
    /*
     * Scrubbed like every other free-text value, and this one is the reason the
     * rule is stated as "every": the health summary names the file being written
     * in full, deliberately, because on screen that path is the one thing
     * somebody can act on. The report is not the screen.
     */
    ['Capture health', redactPathsIn(`${health.state} — ${health.detail}`)],
    ...failureFields(input.view),
    ['Notice', input.notice === undefined ? 'none' : redactPathsIn(input.notice)],
  ];

  return [
    'Clipped diagnostics report',
    '',
    ...renderFields(fields),
    '',
    `Not reported by this build: ${unreported.join(', ')}.`,
    `Log files are not in this report. They are in ${LOG_DIRECTORY}; attach clipped.*.log`,
    'yourself if the problem is a recording that failed (issue #303).',
    'Paths are reduced to a file name and a digest of the whole path, the way Clipped',
    'reduces one in a log line, so no directory component leaves this machine.',
  ].join('\n');
}
