import type { LibrarySession } from '@clipped/shared';
import type { ReactNode } from 'react';

import {
  footageSeconds,
  formatBytes,
  formatDuration,
  formatMoment,
  missingCount,
  presentBytes,
} from './library';

/**
 * The list of sittings, shared by Home and Library.
 *
 * Both screens draw the same row from the same read (issue #301), and a second
 * copy of it would be two places for "this file has gone" to be got wrong
 * (AGENTS.md section 55).
 *
 * # A file that has gone is said, not drawn as a gap
 *
 * The index records `missing_since` for a recording whose file it could not find
 * and never deletes the row (`docs/library.md`). A row for such a recording says
 * so, in words and not by colour alone (AGENTS.md section 46), and its size is
 * left out of the sitting's total — the space it is not occupying is not being
 * used. That is the whole reason the field crosses the process boundary
 * (AGENTS.md section 27).
 *
 * # No thumbnails
 *
 * The deck draws a grid of them. Thumbnails are generated beside the recordings
 * (#57) and this window has no file-system permission to load one, so there is
 * nothing to draw and no placeholder is drawn in their place.
 */

/** What a session list is given. */
export interface SessionListProps {
  /** The sittings, newest first. */
  readonly sessions: readonly LibrarySession[];
  /** What to call the list, for a screen reader. */
  readonly label: string;
}

/** The sittings, one row each. */
export function SessionList({ sessions, label }: SessionListProps): ReactNode {
  return (
    <table className="clipped-table" aria-label={label}>
      <thead>
        <tr>
          <th scope="col">Game</th>
          <th scope="col">Recorded</th>
          <th scope="col">Footage</th>
          <th scope="col">Size</th>
          <th scope="col">Files</th>
        </tr>
      </thead>
      <tbody>
        {sessions.map((session) => (
          <tr key={session.session_id}>
            {/*
             * The catalogue refused to attribute this sitting to a game rather
             * than guessing, and so does this: what to call that group on
             * screen is the screen's decision, and "Not recognised" is it.
             */}
            <td>{session.game_name ?? 'Not recognised'}</td>
            <td>{formatMoment(session.started_at)}</td>
            <td>{formatDuration(footageSeconds(session))}</td>
            <td>{formatBytes(presentBytes(session))}</td>
            <td>{describeFiles(session)}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

/**
 * What a sitting's files amount to, and how many of them have gone.
 *
 * Written out rather than shown as an icon or a colour, because "3 recordings, 1
 * file missing" is the sentence somebody has to read to know their footage was
 * moved or deleted.
 */
function describeFiles(session: LibrarySession): string {
  const count = session.recordings.length;
  const recordings = `${String(count)} recording${count === 1 ? '' : 's'}`;
  const missing = missingCount(session);
  if (missing === 0) {
    return recordings;
  }
  return `${recordings}, ${String(missing)} file${missing === 1 ? '' : 's'} missing`;
}
