import type { ReactNode } from 'react';

import { WaitingOn, type Waiting } from './WaitingOn';

/**
 * The Library screen (issue #60).
 *
 * # Why it lists nothing
 *
 * The deck draws this screen as a Sessions / Clips / Highlights tab strip, a row
 * of filter chips, a search field and a grid of thumbnails. **Not one of those
 * can be honoured in this build**, and the reason is the same for all of them:
 * there is no path from this window to the library index.
 *
 * - `clipped-library` reconciles the session sidecars the recorder writes into
 *   `library.db` (#56, `docs/library.md`), inside the recorder's process. The
 *   control protocol in `docs/ipc.md` defines six commands and none of them
 *   reads a row.
 * - This window cannot open the database itself.
 *   `src-tauri/capabilities/default.json` grants `core:window:allow-set-title`,
 *   `core:event:allow-listen` and `core:event:allow-unlisten`, and Tauri denies
 *   everything not listed.
 * - It may not link the crate either:
 *   `tests/integration/tests/workspace_layering.rs` permits the Tauri host
 *   exactly one member of the workspace, `clipped-ipc`, which is what keeps the
 *   recording engine out of the window's process ([ADR 0002](../../../docs/adr/0002-separate-recorder-process.md)).
 *
 * Issue #301 is that gap, raised by this ticket.
 *
 * So the screen draws no empty grid, no zeroed count and no search field. An
 * empty Sessions tab is indistinguishable from a machine that has recorded
 * nothing, a "0 clips" is a figure nobody measured, and a search box that cannot
 * search is the control AGENTS.md section 27 forbids. What is here instead is
 * what the screen owes and the work that lands each part of it — the same
 * contract the Games screen (#107) keeps.
 *
 * # And why there is no tab strip
 *
 * Issue #215 asks for the deck's tab strip and its selectable chips **at the
 * point a screen needs them**, precisely so that a component with no consumer is
 * not designed against a guess. This screen has nothing to switch between and
 * nothing to filter, so building either here would be the speculative component
 * that issue exists to avoid. They belong with the first screen that has three
 * populated lists, which is this one once #301 lands.
 */

/**
 * Everything SPEC.md sections 17, 29 and 30 ask of the Library screen, against
 * what each one is waiting for.
 *
 * A promise to the reader rather than a derived list: these are the parts of the
 * screen, and each is pinned to the work that lands it.
 */
const WAITING: readonly Waiting[] = [
  {
    shows:
      'Sessions, each with the matches and other footage inside it, as SPEC.md section 17 draws',
    needs:
      'The index that records them (#56, built and uncalled), and a way for this window to read it. Issue #301',
  },
  {
    shows: 'Clips, and the highlights a session produced',
    needs:
      'The virtual clip model (#74), clip creation (#91) and automatic highlights (#76), then the same read. Issue #301',
  },
  {
    shows: 'Search by game, date, event, tag, favourite and duration (SPEC.md section 30)',
    needs:
      'The query language is built and runs inside the recorder (#59). This window needs a way to send a query and receive results. Issue #301',
  },
  {
    shows: 'Favourites, and filtering the list down to them (SPEC.md section 29)',
    needs: 'Favouriting anything at all. Issue #58, then issue #301',
  },
  {
    shows: 'A thumbnail against each recording, and a waveform under each track',
    needs:
      'Thumbnails (#57) and waveforms (#66) are generated beside the files. This window can load neither, having no file-system permission. Issue #301',
  },
  {
    shows: 'A recording whose file has gone, said as that rather than drawn as a broken tile',
    needs:
      'The index already records missing_since for a file it could not find (docs/library.md). It needs reading. Issue #301',
  },
  {
    shows: 'Playing a clip, from the list',
    needs: 'The playback screen. Issue #52',
  },
  {
    shows: 'Restoring something deleted by mistake',
    needs: 'The trash, with its retention and restore. Issue #94',
  },
];

/** The Library screen. */
export function LibraryScreen(): ReactNode {
  return (
    <>
      <h1 className="clipped-screen__title">Library</h1>

      <p className="clipped-screen__lead">
        Every sitting Clipped records leaves video files and one session record beside them, in the
        output folder the recorder was given.
      </p>

      <section className="clipped-panel" aria-label="Why this list is empty">
        <h2 className="clipped-panel__heading">This window cannot read the library</h2>
        <p className="clipped-panel__body">
          The index of sessions, recordings and clips is built inside the recorder&apos;s process.
          The control protocol has no command that reads it, and this window has no file-system
          permission to open it directly, so nothing here has looked at your recordings. Issue #301
          is the work that connects the two.
        </p>
        <p className="clipped-panel__body clipped-muted">
          Nothing is lost in the meantime. The files are ordinary video files and play in anything;
          the session record beside them is JSON, and is what the index is rebuilt from
          (docs/library.md).
        </p>
      </section>

      <h2 className="clipped-screen__heading">What this screen will show</h2>
      <p className="clipped-screen__lead clipped-muted">
        None of it is drawn yet, and none of it is invented in the meantime. Each row names the work
        that supplies it.
      </p>

      <WaitingOn heading="What the Library screen will show" rows={WAITING} />
    </>
  );
}
