import { useCallback, useState, type ReactNode } from 'react';

import {
  buildDiagnosticsReport,
  describeCaptureHealth,
  describeConcerns,
  diagnostics,
  LOG_DIRECTORY,
  NOTHING_HAS_FAILED,
  SCOPE_OF_THIS_SUMMARY,
} from './diagnostics';
import type { RecorderLinkView } from './useRecorderLink';

/**
 * The Diagnostics screen (issue #101, SPEC.md section 36).
 *
 * Three parts, in the order somebody with a problem reads them: whether anything
 * is wrong, what this build can and cannot tell them, and the report they send.
 *
 * # Why there is no Export Support Bundle button
 *
 * SPEC.md section 36 asks for one, and the deck draws one. A bundle worth sending
 * is the log files plus this report, and the log files are on disk at
 * `%LOCALAPPDATA%\Clipped\logs` where **this window cannot reach them**:
 * `src-tauri/capabilities/default.json` grants three `core:` permissions, none of
 * which touches the file system, and there is no command that would read a log or
 * open a folder. A button that opened a save dialog and wrote a report with no
 * logs in it would be an export in name only, and a disabled one would say less
 * than the paragraph that names the issue (AGENTS.md section 27, issue #303).
 *
 * What is here instead is the half that can be done honestly: the report is
 * composed, shown **in full** before it goes anywhere — `docs/privacy.md`,
 * nothing surprising and nothing hidden — and copied to the clipboard, which is
 * where a bug report is pasted from. The screen then names the log directory, so
 * that attaching the logs is something a person can actually do today.
 */

/** What the Diagnostics screen is given. */
export interface DiagnosticsScreenProps {
  /**
   * Everything the window knows about the recorder.
   *
   * The whole view rather than the link alone, because two of its four fields —
   * a recording that failed, and one that was interrupted — are the reason a
   * support report exists, and both are dropped by the state that follows them.
   *
   * Passed in rather than taken from `useRecorderLink` here, so that the shell
   * holds one subscription rather than two and this screen is a pure function of
   * what it is told.
   */
  readonly view: RecorderLinkView;
  /**
   * The last thing the tray or the startup had to report, if anything.
   *
   * It is in the report because it is the only record of a failure that happened
   * before React was running — a notification-area icon that could not be added
   * changes what closing the window does, and nothing else remembers it.
   */
  readonly notice: string | undefined;
}

/** What the clipboard did, as one sentence, or nothing before it was asked. */
type CopyOutcome = string | undefined;

/** The Diagnostics screen. */
export function DiagnosticsScreen({ view, notice }: DiagnosticsScreenProps): ReactNode {
  const health = describeCaptureHealth(view);
  const concerns = describeConcerns(view);

  /*
   * Composed on every render rather than memoised. The report is a description
   * of the state it was composed from, so a render caused by a change in that
   * state must produce a new one — and `takenAt` is when it was composed, which
   * is exactly what the reader needs to interpret it. There is nothing here worth
   * caching: it is a few dozen string concatenations behind a screen somebody
   * opened deliberately.
   */
  const report = buildDiagnosticsReport({
    view,
    notice,
    userAgent: navigator.userAgent,
    takenAt: new Date(),
  });

  const [copied, setCopied] = useState<CopyOutcome>(undefined);

  const copy = useCallback(() => {
    /*
     * The clipboard is a web API rather than a Tauri command, so it needs no
     * entry in `capabilities/default.json` — and it is also not something this
     * code can guarantee: it exists only in a secure context, and it can refuse
     * a write that reaches it. Both are handled, and both say so, because a
     * button that appears to work is the one thing worse than a missing one
     * (AGENTS.md sections 15 and 27). The report is on screen and selectable
     * either way, which is what the sentence points at.
     *
     * **Not verified in the real window.** Doing that means opening one, and
     * this is checked in jsdom against a stub. What is asserted here is the
     * behaviour on both answers, not that WebView2 gives either.
     */
    if (!navigator.clipboard) {
      setCopied(
        'This window has no clipboard access, so nothing was copied. The report above is ' +
          'selectable — select it and copy it by hand.',
      );
      return;
    }

    void navigator.clipboard.writeText(report).then(
      () => {
        setCopied('The report above was copied to the clipboard.');
      },
      (error: unknown) => {
        setCopied(
          `The clipboard refused it: ${String(error)}. The report above is selectable — ` +
            'select it and copy it by hand.',
        );
      },
    );
  }, [report]);

  return (
    <>
      <h1 className="clipped-screen__title">Diagnostics</h1>

      <p className="clipped-screen__lead">
        What Clipped can tell you about the recorder it is talking to, and the report to send with a
        bug report.
      </p>

      {/*
       * A live region, like the recorder status in the sidebar and the Games
       * screen's detection block: a recorder going, or a recording failing,
       * changes what this says while nobody is looking at it.
       */}
      <section className="clipped-panel" aria-label="Capture health" aria-live="polite">
        <h2 className="clipped-panel__heading">{health.state}</h2>
        <p className="clipped-panel__body">{health.detail}</p>
        {health.action !== undefined && <p className="clipped-panel__body">{health.action}</p>}
        {concerns.length === 0 ? (
          <p className="clipped-panel__body">{NOTHING_HAS_FAILED}</p>
        ) : (
          concerns.map((concern) => (
            <p className="clipped-panel__body" key={concern}>
              {concern}
            </p>
          ))
        )}
        <p className="clipped-panel__body clipped-muted">{SCOPE_OF_THIS_SUMMARY}</p>
      </section>

      <h2 className="clipped-screen__heading">What this build reports</h2>
      <p className="clipped-screen__lead clipped-muted">
        SPEC.md section 36 asks a recorder to record all of these. One of them reaches this window.
        The rest are not drawn as empty gauges — a dropped-frame count of zero and a dropped-frame
        count nobody took are different things — so each row names the work that supplies it.
      </p>

      <table className="clipped-table">
        <thead>
          <tr>
            <th scope="col">Diagnostic</th>
            <th scope="col">What this build reports</th>
          </tr>
        </thead>
        <tbody>
          {diagnostics(view).map((entry) => (
            <tr key={entry.subject}>
              <td>{entry.subject}</td>
              <td className={entry.known ? undefined : 'clipped-muted'}>{entry.reported}</td>
            </tr>
          ))}
        </tbody>
      </table>

      <h2 className="clipped-screen__heading">Support report</h2>
      <p className="clipped-screen__lead">
        This is the whole of it. Nothing is collected that is not below, nothing is sent anywhere,
        and every path is reduced to a file name and a digest of the whole path — the same reduction
        Clipped applies before writing a path to a log — so no folder name, drive or account name
        leaves this machine.
      </p>
      <p className="clipped-screen__lead">
        It contains no recorded media, no window title, no microphone audio and no file contents,
        because none of those reaches this window in the first place.
      </p>

      <pre className="clipped-screen__report">{report}</pre>

      <button type="button" className="clipped-btn clipped-btn--primary" onClick={copy}>
        Copy report
      </button>
      {/*
       * A live region rather than a sentence that appears silently: the button's
       * whole outcome is this line, and a keyboard user who cannot see it has no
       * other way to know whether the clipboard took it.
       */}
      <p className="clipped-screen__lead" role="status">
        {copied}
      </p>

      <p className="clipped-screen__lead clipped-muted">
        The log files are the other half of a support bundle and are not in the report: they are in{' '}
        {LOG_DIRECTORY}, rotated hourly with the newest 48 kept, and this window can neither read
        them nor open the folder. Attach clipped.*.log yourself when the problem is a recording that
        failed. Writing one archive with both in it is issue #303.
      </p>
    </>
  );
}
