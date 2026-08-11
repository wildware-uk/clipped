import type { ReactNode } from 'react';

/** The recorder status block at the foot of the sidebar. */
export interface RecorderStatusProps {
  /** The state in two or three words, e.g. "Not connected". */
  readonly state: string;
  /** One sentence saying what that means for the person reading it. */
  readonly detail: string;
}

/**
 * What the recorder process is doing, always on screen.
 *
 * The state is a word and a mark, never a colour on its own (AGENTS.md section
 * 46), and it is a live region so that a change is announced rather than only
 * drawn. There are no controls here yet: the shell cannot talk to the recorder
 * until the IPC protocol exists (issue #49), and a Start Recording button that
 * did nothing would be worse than no button (AGENTS.md section 27).
 */
export function RecorderStatus({ state, detail }: RecorderStatusProps): ReactNode {
  return (
    <section className="clipped-status" aria-label="Recorder status">
      <p className="clipped-kicker">Recorder</p>
      <p className="clipped-status__state" aria-live="polite">
        <span className="clipped-status__marker" aria-hidden="true" />
        <span>{state}</span>
      </p>
      <p className="clipped-status__detail">{detail}</p>
    </section>
  );
}
