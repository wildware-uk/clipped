import { useCallback, useState, type ReactNode } from 'react';

import {
  buildDiagnosticsReport,
  describeCaptureHealth,
  describeConcerns,
  describeEncoderSource,
  diagnostics,
  LOG_DIRECTORY,
  NOTHING_HAS_FAILED,
  SCOPE_OF_THIS_SUMMARY,
} from './diagnostics';
import { describeDiagnosticsProblem, useRecorderDiagnostics } from './recorderDiagnostics';
import type { RecorderLinkView } from './useRecorderLink';

/**
 * The Diagnostics screen (issue #101, SPEC.md section 36).
 *
 * Four parts, in the order somebody with a problem reads them: whether anything
 * is wrong, what this build can and cannot tell them, what this machine can
 * encode, and the report they send.
 *
 * # Where the middle two come from
 *
 * Four of SPEC.md section 36's twelve reach this window. Two of them arrive
 * inside a status it was already subscribed to — the recording's path, and the
 * game the open sitting is of — and two are asked for once when this screen
 * opens: the capture backend of the recording in progress, and the capability
 * report `clipped-recorder capabilities` prints
 * (`recorderDiagnostics.ts`, issue #302). The other nine name the work that
 * would measure them, which is what this screen does instead of drawing gauges
 * at zero.
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
   * Asked once, when this screen opens. Both answers are settled before this
   * window is likely to exist — a recording chooses its backend in its first
   * milliseconds, and the capability report is read when the recorder starts —
   * so there is no event to wait for and a window that waited for one would show
   * an empty screen for ever (`recorderDiagnostics.ts`, issue #302).
   */
  const recorder = useRecorderDiagnostics();

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
    recorder,
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
        SPEC.md section 36 asks a recorder to record all of these. Four of them reach this window.
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
          {diagnostics(view, recorder).map((entry) => (
            <tr key={entry.subject}>
              <td>{entry.subject}</td>
              <td className={entry.known ? undefined : 'clipped-muted'}>{entry.reported}</td>
            </tr>
          ))}
        </tbody>
      </table>

      <h2 className="clipped-screen__heading">What this machine can encode</h2>
      {recorder.state === 'reading' && (
        <p className="clipped-screen__lead clipped-muted">Asking the recorder.</p>
      )}
      {recorder.state === 'unread' && (
        <p className="clipped-screen__lead">{describeDiagnosticsProblem(recorder.problem)}</p>
      )}
      {recorder.state === 'read' && (
        <>
          <p className="clipped-screen__lead clipped-muted">
            {describeEncoderSource(recorder.value.encoders)}
          </p>

          <table className="clipped-table">
            <thead>
              <tr>
                <th scope="col">Adapter</th>
                <th scope="col">What it is</th>
              </tr>
            </thead>
            <tbody>
              {recorder.value.encoders.adapters.map((adapter) => (
                <tr key={adapter.description}>
                  <td>{adapter.description}</td>
                  <td>
                    {adapter.vendor}, {adapter.kind.replaceAll('_', ' ')}
                    {adapter.driver_version === undefined
                      ? ''
                      : `, driver ${adapter.driver_version}`}
                    {adapter.captures ? '. Recordings capture on this one.' : ''}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>

          <table className="clipped-table">
            <thead>
              <tr>
                <th scope="col">Encoder</th>
                <th scope="col">Where it stands</th>
                <th scope="col">Codecs</th>
              </tr>
            </thead>
            <tbody>
              {recorder.value.encoders.encoders.map((encoder) => (
                <tr key={encoder.encoder}>
                  <td>{encoder.label}</td>
                  {/*
                   * Three things a row can be, and they are deliberately not
                   * collapsed into a tick: an encoder this machine has and this
                   * build cannot drive is not the same as one that is not here,
                   * and a report of green ticks from a build that can use half of
                   * them would be worse than no report (AGENTS.md sections 27
                   * and 54).
                   */}
                  <td className={encoder.available ? undefined : 'clipped-muted'}>
                    {encoder.unavailable ??
                      (encoder.implemented
                        ? 'Available, and this build can record with it.'
                        : 'Available, and this build has no backend proven to record with it.')}
                  </td>
                  <td className={encoder.codecs.length === 0 ? 'clipped-muted' : undefined}>
                    {encoder.codecs.length === 0
                      ? 'None: an encoder that is not here has no codecs.'
                      : encoder.codecs
                          .map((codec) => {
                            const size =
                              codec.max_width === undefined || codec.max_height === undefined
                                ? ''
                                : ` up to ${String(codec.max_width)}×${String(codec.max_height)}`;
                            const rate =
                              codec.max_framerate_1080p === undefined
                                ? ''
                                : ` at ${String(codec.max_framerate_1080p)} fps at 1080p`;
                            /*
                             * The one thing this table has to make obvious: a
                             * figure from a vendor's published table is true of
                             * the hardware that table covers and is not a promise
                             * about this card.
                             */
                            const evidence = codec.inferred ? ' (inferred)' : '';
                            return `${codec.codec}${size}${rate}${evidence}`;
                          })
                          .join('; ')}
                  </td>
                </tr>
              ))}
            </tbody>
          </table>
        </>
      )}

      {/*
       * Which FFmpeg is loaded, where somebody can read it without a terminal
       * (issue #256). The recorder logs it at start-up, which answers it for
       * anybody who can find the log file; this is the same fact for everybody
       * else, and it is the acceptance criterion that was outstanding.
       *
       * The licence comes from the libraries rather than from a constant here.
       * The DLLs beside the binaries are replaceable by design — the LGPL
       * relinking permission — and a substituted GPL build reports the same
       * version numbers and a different licence, so a hardcoded "LGPL" would be
       * the one thing this must not say.
       */}
      {/*
       * A library that will not open, said where somebody can see it.
       *
       * This is first on the screen, above the encoders and the FFmpeg build,
       * because it is the one reading here that means something is wrong *now*.
       * The others describe a working recorder; this one says recordings are
       * being made and none of them is reaching the library — the Library
       * screen empty, the recent clips empty, and automatic cleanup not running,
       * because all three read the index.
       *
       * On the machine that raised issue #738 a renumbered migration stopped the
       * library opening for four days, and the only evidence anywhere was one
       * `WARN` an hour in a log file. The user concluded the recorder did not
       * work while it was recording perfectly (AGENTS.md section 27).
       *
       * The recorder's sentence is shown verbatim. It names the migration that
       * failed, which is what anybody diagnosing this needs, and a summary
       * written here would be a second description of a fault this screen
       * cannot diagnose.
       */}
      {recorder.state === 'read' && recorder.value.library_refusal !== undefined && (
        <>
          <h2 className="clipped-screen__heading">Recordings are not reaching the library</h2>
          <p className="clipped-screen__lead">
            Recording itself is unaffected and the files are being written. What has stopped is the
            index the Library screen reads, so recordings made while this is true will not be listed
            until it is working again.
          </p>
          <table className="clipped-table" aria-label="Library index">
            <tbody>
              <tr>
                <th scope="row">Why</th>
                <td>{recorder.value.library_refusal}</td>
              </tr>
            </tbody>
          </table>
        </>
      )}

      {recorder.state === 'read' && recorder.value.ffmpeg !== undefined && (
        <>
          <h2 className="clipped-screen__heading">The FFmpeg this recorder loaded</h2>
          <table className="clipped-table" aria-label="FFmpeg build">
            <tbody>
              <tr>
                <th scope="row">Build</th>
                <td>{recorder.value.ffmpeg.identifier}</td>
              </tr>
              <tr>
                <th scope="row">Licence</th>
                <td>{recorder.value.ffmpeg.licence}</td>
              </tr>
              <tr>
                <th scope="row">Libraries</th>
                <td>
                  avformat {recorder.value.ffmpeg.avformat}, avcodec {recorder.value.ffmpeg.avcodec}
                  , avutil {recorder.value.ffmpeg.avutil}
                </td>
              </tr>
            </tbody>
          </table>
        </>
      )}

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
