import { Dialog } from '@clipped/ui';
import type { ReactNode } from 'react';

import type { EditDocument } from './document';
import {
  checksNeedingTheRecording,
  copyOutlook,
  describeBlocker,
  type DocumentBlocker,
} from './exportPlan';
import { formatTimecode } from './timeline';

/**
 * The export dialog (issue #90).
 *
 * # What it is for
 *
 * The export engine (#89) implements one method — a **stream copy**, which
 * writes the recording's own coded packets — and refuses everything else,
 * naming every reason (`docs/exporting.md`). A dialog in front of an engine
 * like that has one job, and it is not collecting settings: it is saying what
 * is about to happen, before somebody waits for it (AGENTS.md sections 27 and
 * 45). An export that will be a fast lossless copy should say so; one that
 * cannot be done at all has to say which edit is responsible, in words the
 * person can act on, rather than failing after they have pressed Export.
 *
 * # Why there is no resolution, framerate, codec or quality
 *
 * All four are properties of a re-encode, and there is no re-encode: an edit
 * that needs one is refused rather than exported (`docs/exporting.md`, "What is
 * not built yet"). A copy has no settings by definition — it writes what the
 * recording holds. So the deck's preset chips are not drawn: a control that
 * silently does nothing is exactly what AGENTS.md section 27 forbids, and a
 * quality picker over a lossless copy would be the worst kind of one, because
 * its label implies the file it produced was affected by it.
 *
 * # Why there is no Export button
 *
 * Nothing in this window can start an export. The control protocol has no
 * command about one, the window has no file-system permission to write a file
 * with or to choose a destination through, and it may not link `clipped-export`
 * — [#322](https://github.com/wildware-uk/clipped/issues/322) is that gap. The
 * dialog says so, in the one paragraph on it that has to be read, rather than
 * offering a button that would fail.
 *
 * # Why there is no estimated size, and no progress bar
 *
 * `ExportPlan` reports the method, the blockers, the segments, the audio tracks
 * and the duration. It reports no bytes, and nothing in this window has counted
 * any, so a size here would be a figure nobody measured;
 * [#323](https://github.com/wildware-uk/clipped/issues/323) is what makes it
 * answerable. Progress and cancellation are the engine's (`ExportProgress`,
 * `Cancellation`) and arrive with #322: a progress bar with nothing behind it
 * is the same bug as a cancel button that does nothing.
 */

/** What the dialog is given. */
export interface ExportDialogProps {
  /** The clip an export would render. */
  readonly clip: EditDocument;
  /** How long that clip is, in nanoseconds; the screen has already read it. */
  readonly durationNanos: number;
  /** Called when the dialog is dismissed. */
  readonly onClose: () => void;
}

/** The export dialog. */
export function ExportDialog({ clip, durationNanos, onClose }: ExportDialogProps): ReactNode {
  const outlook = copyOutlook(clip);

  return (
    <Dialog
      title="Export clip"
      onClose={onClose}
      actions={
        <button type="button" className="clipped-btn clipped-btn--secondary" onClick={onClose}>
          Close
        </button>
      }
    >
      <p className="clipped-dialog__body">
        {clip.title} — {formatTimecode(durationNanos)} in{' '}
        {clip.segments.length === 1 ? '1 segment' : `${String(clip.segments.length)} segments`}.
      </p>

      {outlook.ok ? (
        <Outlook clip={clip} blockers={outlook.blockers} />
      ) : (
        <section aria-label="What an export would do">
          <h3 className="clipped-editor__export-heading">
            This clip cannot be exported as it stands
          </h3>
          <p className="clipped-dialog__body">{outlook.problem}</p>
        </section>
      )}

      <section className="clipped-panel" aria-label="Starting an export">
        <h3 className="clipped-panel__heading">No export can be started here yet</h3>
        <p className="clipped-panel__body">
          The engine is built and this window cannot reach it: the recorder’s control protocol has
          no command about an export, and this window can neither choose a destination nor write a
          file. Issue #322.
        </p>
      </section>
    </Dialog>
  );
}

/** What the document settles about copying this clip, and what it does not. */
function Outlook({
  clip,
  blockers,
}: {
  readonly clip: EditDocument;
  readonly blockers: readonly DocumentBlocker[];
}): ReactNode {
  if (blockers.length > 0) {
    return (
      <section aria-label="What an export would do">
        <h3 className="clipped-editor__export-heading">Clipped cannot export this clip yet</h3>
        <p className="clipped-dialog__body">
          An export copies the recording’s own coded packets, and these edits rule that out.
          Producing them instead needs re-encoding, which is not built (issue #89), so the export is
          refused rather than made slowly.
        </p>
        <ul className="clipped-editor__reasons">
          {blockers.map((blocker) => {
            const reason = describeBlocker(clip, blocker);
            return (
              <li key={`${blocker.kind}-${reason.what}`}>
                {reason.what} <span className="clipped-dialog__body">{reason.change}</span>
              </li>
            );
          })}
        </ul>
      </section>
    );
  }

  return (
    <section aria-label="What an export would do">
      <h3 className="clipped-editor__export-heading">Nothing in this edit rules out a fast copy</h3>
      <p className="clipped-dialog__body">
        A copy writes the recording’s own coded frames, bit for bit: as fast as reading the file,
        with no second generation of the picture. The recording itself is never modified, moved or
        re-encoded because you exported a clip.
      </p>
      <p className="clipped-dialog__body">
        Whether this clip is one also depends on the recording, which this window cannot open (issue
        #322):
      </p>
      <ul className="clipped-editor__reasons">
        {checksNeedingTheRecording(clip).map((check) => (
          <li key={check} className="clipped-dialog__body">
            {check}
          </li>
        ))}
      </ul>
      <p className="clipped-dialog__body">
        If one of those does not hold, the export is refused rather than re-encoded, and it names
        which one.
      </p>
    </section>
  );
}
