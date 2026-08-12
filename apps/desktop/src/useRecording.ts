import type { RecorderStatus, RecordingSummary } from '@clipped/shared';
import { invoke } from '@tauri-apps/api/core';
import { useCallback, useEffect, useRef, useState } from 'react';

import type { RecorderProblem, RecordTarget, StartedRecording } from './recording';
import { inTauriWindow, type RecorderLinkState } from './useRecorderLink';

/**
 * Starting and stopping a recording, and knowing whether one is running
 * ([issue #389](https://github.com/wildware-uk/clipped/issues/389)).
 *
 * # Why this asks, rather than remembers
 *
 * The link already carries a status, and it is not good enough for this. The
 * recorder publishes `status_changed` when a recording starts and when it ends
 * and at no point between (`apps/recorder/src/serve.rs`), so the `elapsed_ms`
 * inside it is the elapsed time at the moment the recording began and never
 * moves. Counting up from it here would be a figure nobody measured (AGENTS.md
 * section 27).
 *
 * Worse than a frozen number: a window that decided it was recording — because
 * the start command resolved, or because a timer of its own was running — goes
 * on saying so after the recorder has died. That is the specific failure #389
 * forbids, and the reason **nothing in this hook writes a recording state**.
 * `start` and `stop` send a command and record only whether it was refused; the
 * recording state on screen is whatever the last `get_status` answered, and a
 * recorder that stops answering takes it away within one interval.
 *
 * # What it costs
 *
 * One named-pipe round trip a second, and only while the Home screen is mounted
 * — the hook is called from there rather than from the shell, so leaving the
 * screen stops the asking. `get_status` reads a mutex and serialises four
 * fields; it does not touch the capture threads (AGENTS.md section 18).
 */

/**
 * How often the window asks the recorder what it is doing.
 *
 * A second, because that is the resolution of the elapsed time it draws: asking
 * more often would redraw the same figure, and asking less often would show a
 * duration that visibly stutters.
 */
export const STATUS_INTERVAL_MS = 1_000;

/** The code a failure that never got as far as the recorder carries. */
const WINDOW_FAULT = 'window_fault';

/** Everything the record control needs, and the two things it can do. */
export interface RecordingView {
  /**
   * What the recorder last said it was doing, or `null` when it has not
   * answered — which is not the same as idle, and is drawn differently.
   *
   * `null` again the moment an ask fails, because a recorder that cannot be
   * asked is not a recorder that is recording.
   */
  readonly status: RecorderStatus | null;
  /** Why the last ask failed, or `null` if the last one was answered. */
  readonly problem: RecorderProblem | null;
  /** What pressing record would record, or `null` if nothing would. */
  readonly target: RecordTarget | null;
  /**
   * Why the last start or stop was refused, in the recorder's own words.
   *
   * Kept until the next press, because it stays true: a recording that could
   * not start is not undone by the next status arriving, and a refusal that
   * vanished within the second would be one nobody read (AGENTS.md section 45).
   */
  readonly refusal: RecorderProblem | null;
  /**
   * The recording this window last stopped, as the recorder finished it.
   *
   * The file is the point. It is the only thing on the screen anybody can act
   * on, and the panel would otherwise go straight from "Recording cs2.exe" to
   * "not recording" and take the path with it — leaving the user with a file
   * they just made and no idea where it is.
   */
  readonly finished: RecordingSummary | null;
  /**
   * Whether a start or stop is in flight.
   *
   * Deliberately **not** a recording state. It disables the button so that a
   * second press cannot send a second command while `stop_recording` is
   * finalising a file, and it is never what makes the screen say "recording".
   */
  readonly working: boolean;
  /** Asks the recorder to record {@link RecordingView.target}. */
  readonly start: () => void;
  /** Asks the recorder to stop the recording this window has on screen. */
  readonly stop: () => void;
}

/**
 * What a rejected command carries, or a description of whatever else it was.
 *
 * Tauri rejects with the serialised `Err` value, which for these commands is
 * `RecorderProblem`. Anything else is a fault in this window rather than a
 * refusal by the recorder, and is given a code of its own so the two cannot be
 * confused — the same distinction the Rust side draws between a refusal and an
 * unreachable recorder.
 */
function asProblem(error: unknown): RecorderProblem {
  if (
    typeof error === 'object' &&
    error !== null &&
    'code' in error &&
    'message' in error &&
    typeof (error as { code: unknown }).code === 'string' &&
    typeof (error as { message: unknown }).message === 'string'
  ) {
    return error as RecorderProblem;
  }

  return { code: WINDOW_FAULT, message: `Clipped could not ask the recorder: ${String(error)}` };
}

/**
 * Follows what the recorder is doing, and offers the two things to do about it.
 *
 * `link` decides whether there is anything to ask: the status round trip runs
 * only while the window is attached, so a build with no recorder is not opening
 * a pipe every second to be told again that there is none. The reason there is
 * none is already on screen, from the link.
 */
export function useRecording(link: RecorderLinkState | null): RecordingView {
  const [status, setStatus] = useState<RecorderStatus | null>(null);
  const [problem, setProblem] = useState<RecorderProblem | null>(null);
  const [target, setTarget] = useState<RecordTarget | null>(null);
  const [refusal, setRefusal] = useState<RecorderProblem | null>(null);
  const [finished, setFinished] = useState<RecordingSummary | null>(null);
  const [working, setWorking] = useState(false);

  const attached = link?.link === 'attached';

  /*
   * The current round of asking, so that a press can bring the next one forward
   * rather than leaving the screen a second behind the thing that just
   * happened. A ref because `start` and `stop` must not be rebuilt every time
   * the poll answers, and because the function it holds is replaced whenever the
   * effect below re-runs.
   */
  const askNow = useRef<() => void>(() => undefined);

  useEffect(() => {
    if (!inTauriWindow()) {
      return;
    }

    let current = true;
    let timer: ReturnType<typeof setTimeout> | undefined;

    /*
     * What would be recorded, every round: the foreground window changes while
     * this screen is open, and a button naming an application the user has
     * since left would record the wrong thing. It opens no pipe — the Rust side
     * reads a value it already holds.
     */
    const askWhatToRecord = (): Promise<void> =>
      invoke<RecordTarget | null>('record_target')
        .then((answer) => {
          if (current) {
            setTarget(answer);
          }
        })
        .catch(() => {
          // A window that cannot find out what to record offers no start, which
          // `describeRecordControl` draws from `target` being null.
          if (current) {
            setTarget(null);
          }
        });

    /**
     * What the recorder is doing, when there is one to ask.
     *
     * Detached, the answer is that this window does not know, which is `null` —
     * not idle. The link is already saying why on the same screen.
     */
    const askWhatIsHappening = (): Promise<void> => {
      if (!attached) {
        setStatus(null);
        return Promise.resolve();
      }

      return invoke<RecorderStatus>('recorder_status')
        .then((answer) => {
          if (current) {
            setStatus(answer);
            setProblem(null);
          }
        })
        .catch((error: unknown) => {
          if (current) {
            // The half of this hook that #389 is about. A recorder that could
            // not be asked is not a recorder that is recording, so the state it
            // was last seen in is dropped rather than left on screen.
            setStatus(null);
            setProblem(asProblem(error));
          }
        });
    };

    /*
     * The next round is scheduled when this one has finished rather than on a
     * fixed interval, so that a slow answer cannot leave two asks in flight —
     * `stop_recording` blocks until a file is finalised, and a pile of queued
     * status requests behind it would all arrive at once.
     */
    const ask = (): void => {
      void Promise.all([askWhatToRecord(), askWhatIsHappening()]).then(() => {
        if (current) {
          timer = setTimeout(ask, STATUS_INTERVAL_MS);
        }
      });
    };

    askNow.current = (): void => {
      clearTimeout(timer);
      ask();
    };

    ask();

    return () => {
      current = false;
      clearTimeout(timer);
      askNow.current = (): void => undefined;
    };
  }, [attached]);

  const start = useCallback(() => {
    if (target === null) {
      return;
    }

    setWorking(true);
    setRefusal(null);
    setFinished(null);

    invoke<StartedRecording>('start_recording', { processId: target.process_id })
      // Deliberately nothing on success. What the screen says next comes from
      // the recorder's own answer to `get_status`, which the ask below brings
      // forward; a `setStatus` here would be the window assuming the thing it
      // asked for happened.
      .then(() => undefined)
      .catch((error: unknown) => {
        setRefusal(asProblem(error));
      })
      .finally(() => {
        setWorking(false);
        askNow.current();
      });
  }, [target]);

  const stop = useCallback(() => {
    /*
     * The recording this window has on screen, so that one which ended by
     * itself in the meantime cannot have its successor stopped instead
     * (`StopRecording::recording_id`, `docs/ipc.md`). `null` when there is
     * none, which the recorder reads as "whatever is running".
     */
    const recordingId = status?.state === 'recording' ? status.recording_id : null;

    setWorking(true);
    setRefusal(null);

    invoke<RecordingSummary>('stop_recording', { recordingId })
      .then((summary) => {
        // Not a recording state: the reply says the file is finished and where
        // it is, and that is all this holds. Whether anything is recording
        // still comes from the next `get_status`.
        setFinished(summary);
      })
      .catch((error: unknown) => {
        setRefusal(asProblem(error));
      })
      .finally(() => {
        setWorking(false);
        askNow.current();
      });
  }, [status]);

  return { status, problem, target, refusal, finished, working, start, stop };
}
