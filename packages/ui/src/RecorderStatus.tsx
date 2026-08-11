import type { ReactNode } from 'react';

/** The recorder status block at the foot of the sidebar. */
export interface RecorderStatusProps {
  /** The state in two or three words, e.g. "Not connected". */
  readonly state: string;
  /** One sentence saying what that means for the person reading it. */
  readonly detail: string;
  /**
   * Something that happened and is still worth reading, or nothing.
   *
   * Deliberately not part of `detail`. `detail` describes the state the
   * recorder is in *now* and is replaced whenever that changes; a notice is an
   * event that stays true afterwards — a recording that was interrupted left a
   * file, and the file is still there whatever the recorder is doing a second
   * later.
   *
   * `| undefined` explicitly, because `exactOptionalPropertyTypes` is on in
   * `tsconfig.base.json`: without it a caller could omit the property but not
   * pass the absence of one, and "there is no notice" is a value this block's
   * caller computes rather than a branch it takes.
   */
  readonly notice?: string | undefined;
}

/**
 * What the recorder process is doing, always on screen.
 *
 * The state is a word and a mark, never a colour on its own (AGENTS.md section
 * 46), and it is a live region so that a change is announced rather than only
 * drawn. There are no controls here yet: the shell cannot talk to the recorder
 * until the IPC protocol exists (issue #49), and a Start Recording button that
 * did nothing would be worse than no button (AGENTS.md section 27).
 *
 * A `notice` is drawn below the state, marked with a rule rather than only with
 * colour, and carries `role="status"` so that a screen reader hears it when it
 * appears. It is nothing that a sighted user has to be looking at the sidebar to
 * have seen: issue #110 is the notification that reaches somebody playing a
 * game full screen.
 */
export function RecorderStatus({ state, detail, notice }: RecorderStatusProps): ReactNode {
  return (
    <section className="clipped-status" aria-label="Recorder status">
      <p className="clipped-kicker">Recorder</p>
      <p className="clipped-status__state" aria-live="polite">
        <span className="clipped-status__marker" aria-hidden="true" />
        <span>{state}</span>
      </p>
      <p className="clipped-status__detail">{detail}</p>
      {notice === undefined ? null : (
        <p className="clipped-status__notice" role="status">
          {notice}
        </p>
      )}
    </section>
  );
}
