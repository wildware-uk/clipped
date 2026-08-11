import type { JSX } from 'react';

/**
 * The foot of the sidebar, where the design shows what the recorder is doing.
 *
 * It shows the only thing that is true today: this build of the application
 * has no way to reach the recorder. The protocol it will speak is issue #49,
 * and the supervised recorder process it will speak to is issue #106. Until
 * one of them exists, an elapsed time or a Start Recording button here would
 * be a control that silently does nothing (AGENTS.md sections 27 and 54).
 *
 * The state is not colour-coded, because there is nothing to compare it
 * against yet; the hollow mark is the same shape the running state will use
 * filled (AGENTS.md section 46).
 */
export function RecorderStatus(): JSX.Element {
  return (
    <div className="recorder-status">
      <p className="recorder-status-label">
        <span className="recorder-status-mark" aria-hidden="true" />
        Recorder unavailable
      </p>
      <p className="recorder-status-detail">
        This build cannot reach the recording process. Record from the command line with{' '}
        <code>clipped-recorder record</code>.
      </p>
    </div>
  );
}
