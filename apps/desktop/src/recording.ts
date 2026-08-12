import type { RecorderStatus } from '@clipped/shared';

import type { RecorderLinkState } from './useRecorderLink';

/**
 * The record control's vocabulary: what it would record, what it says, and how
 * long the recording it is watching has been running.
 *
 * Beside `useRecording.ts` rather than inside it, for the reason
 * `recordingNow.ts` sits beside the Home screen: the wording and the arithmetic
 * are the parts worth testing, and a module exporting both a hook and a pure
 * function is one neither Fast Refresh nor a reader can take apart.
 *
 * # The rule every function here follows
 *
 * **Nothing is derived from the fact that a button was pressed.** A press sends
 * a command; what the screen then says comes from the recorder's answer to
 * `get_status` and from nothing else. A window that set "recording" when the
 * start command resolved would go on saying so after the recorder died, which
 * is the one failure [issue #389](https://github.com/wildware-uk/clipped/issues/389)
 * names — and it is why {@link describeRecordControl} takes a status rather
 * than a flag.
 */

/**
 * A request to the recorder that did not produce the reply it asked for.
 *
 * `RecorderProblem` in `src-tauri/src/main.rs`, which is what the Rust side
 * serialises when a `#[tauri::command]` returns `Err`. Not a protocol message —
 * no recorder ever sends one — so it is written here rather than in
 * `packages/shared`, the same division `RecorderLinkState` follows.
 */
export interface RecorderProblem {
  /**
   * The recorder's own protocol code — `already_recording`, `not_recording`,
   * `target_not_found`, `unknown_command` — or one of the Rust side's three for
   * a question that never reached a recorder.
   */
  readonly code: string;
  /** One sentence for a person, which is the part that is always shown. */
  readonly message: string;
}

/**
 * What the record control would record, if it were pressed now.
 *
 * `RecordTarget` in `src-tauri/src/main.rs`: the application the user was last
 * in, which is the same answer the tray's Start Recording gives and comes from
 * the same place. `null` is a real state — a machine just signed into has had
 * nothing in front of it — and is why the control can be disabled with a reason
 * rather than being drawn as though it would work.
 */
export interface RecordTarget {
  /** The process `start_recording` would be given. */
  readonly process_id: number;
  /** The executable's file name, such as `cs2.exe`, for the button to name. */
  readonly process_name: string;
}

/** A recording that started: `StartedRecording` in `src-tauri/src/main.rs`. */
export interface StartedRecording {
  /** Identifies it to `stop_recording`. */
  readonly recording_id: string;
  /** The file it is writing. */
  readonly output: string;
}

/**
 * How long a recording has been running, as `M:SS` or `H:MM:SS`.
 *
 * Truncated rather than rounded, because a recording that has run for 1,900
 * milliseconds has completed one second and is part way through the second: a
 * duration that rounded up would show `0:02` beside a file holding one second
 * of video.
 *
 * Hours appear only once there are some. `0:04:07` for a four-minute recording
 * is a clock rather than a duration, and the leading zero is the part somebody
 * has to read past every time.
 */
export function formatElapsed(milliseconds: number): string {
  const total = Math.max(0, Math.floor(milliseconds / 1000));
  const seconds = total % 60;
  const minutes = Math.floor(total / 60) % 60;
  const hours = Math.floor(total / 3600);
  const secondsText = String(seconds).padStart(2, '0');

  return hours > 0
    ? `${String(hours)}:${String(minutes).padStart(2, '0')}:${secondsText}`
    : `${String(minutes)}:${secondsText}`;
}

/** What the record button says, and what pressing it would do. */
export interface RecordControl {
  /** The button's label, which names what it would record where it can. */
  readonly label: string;
  /**
   * What a press sends, or `null` when the button is disabled.
   *
   * `null` is what makes a disabled button honest rather than decorative: there
   * is no branch in which a press with no action sends anything (AGENTS.md
   * section 27).
   */
  readonly action: 'start' | 'stop' | null;
  /**
   * Why it cannot be pressed, when it cannot.
   *
   * Always rendered beside the button rather than left to a `title` attribute: a
   * tooltip is invisible to a keyboard and to a screen reader, and a disabled
   * control with no stated reason is the one AGENTS.md section 45 is about. The
   * tray puts its reason inside the item's own label because a
   * notification-area menu has nowhere else to put one; a screen has somewhere,
   * and uses it.
   */
  readonly reason?: string;
}

/**
 * What the record button should say, given where the link stands and what the
 * recorder last said it was doing.
 *
 * The five disabled cases are listed rather than collapsed, because each has a
 * different thing to tell the user, and one shared "cannot record now" would
 * lose all of it. They mirror `record_entry` in `src-tauri/src/tray_model.rs`,
 * which makes the same decision for the notification-area menu.
 *
 * `status` is the recorder's own answer to `get_status`. `null` means it has not
 * answered yet — not that it is idle — and the two are drawn differently: a
 * button offering to start a recording that may already be running would send a
 * command the recorder is about to refuse.
 */
export function describeRecordControl(
  link: RecorderLinkState | null,
  status: RecorderStatus | null,
  target: RecordTarget | null,
): RecordControl {
  /*
   * The reasons below are deliberately short, and deliberately do not repeat
   * the link's own: the panel this button sits in has just given that in full,
   * immediately above it, and a screen that says the same thing twice is harder
   * to read rather than clearer (AGENTS.md section 28). What is added here is
   * the consequence for *this control*, which the panel does not say.
   */
  if (link === null || link.link === 'unavailable') {
    return {
      label: 'Start recording',
      action: null,
      reason: 'There is no recorder to record with.',
    };
  }

  if (link.link !== 'attached') {
    return {
      label: 'Start recording',
      action: null,
      reason: 'This window is still looking for the recorder.',
    };
  }

  if (status === null) {
    return {
      label: 'Start recording',
      action: null,
      reason: 'Waiting for the recorder to say what it is doing.',
    };
  }

  if (status.state === 'recording') {
    return { label: 'Stop recording', action: 'stop' };
  }

  if (target === null) {
    return {
      label: 'Start recording',
      action: null,
      reason:
        'Nothing has been in front of this window to record. Open the game you want recorded, ' +
        'then come back.',
    };
  }

  return { label: `Start recording ${target.process_name}`, action: 'start' };
}
