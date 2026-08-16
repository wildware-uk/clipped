import type { LibraryRecording, LibrarySession } from '@clipped/shared';
import type { ReactNode } from 'react';

import {
  footageSeconds,
  formatBytes,
  formatDuration,
  formatMoment,
  missingCount,
  presentBytes,
} from './library';
import type { Favourites, FavouriteTarget } from './favourites';
import { canActOn, fileName, type RecordingActions } from './recordingActions';

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
 * # Why the recordings are only listed when there is something to do with them
 *
 * `actions` is what turns a sitting into a sitting **and its files** (issue
 * #399). Home passes none: it is a summary of what has been recorded lately,
 * and a list of file names under each sitting would bury it. The Library passes
 * the three things a recording can have done to it, and each one is drawn
 * against the file it acts on, because "Export" with no subject exports
 * something the user did not choose.
 *
 * A recording whose file has gone gets the same three controls **disabled**,
 * rather than hidden: hiding them would leave a row with nothing on it and no
 * explanation, and a disabled control that says why is what AGENTS.md sections
 * 27 and 45 ask for.
 *
 * # Favouriting is offered for a recording whose file has gone
 *
 * Deliberately, and unlike the three above. A favourite is a statement about
 * what to keep, and the moment it matters most is a recording somebody is about
 * to go looking for: marking it protects the row from automatic cleanup
 * (`docs/library.md`) whether or not the file is where the index last saw it.
 * The three actions need the file; this does not.
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
  /**
   * What can be done with each recording, where anything can.
   *
   * Absent draws the sittings alone, which is what Home wants.
   */
  readonly actions?: RecordingActions;
  /**
   * Marking a sitting or a recording as one to keep (issue #58).
   *
   * Absent draws no stars at all. Home passes none for the same reason it
   * passes no actions: it is a summary of what has been recorded lately, and
   * the Library is where a sitting is acted on.
   */
  readonly favourites?: Favourites;
}

/** The sittings, one row each, and their recordings under them. */
export function SessionList({ sessions, label, actions, favourites }: SessionListProps): ReactNode {
  return (
    <table className="clipped-table" aria-label={label}>
      <thead>
        <tr>
          <th scope="col">Game</th>
          <th scope="col">Recorded</th>
          <th scope="col">Footage</th>
          <th scope="col">Size</th>
          <th scope="col">Files</th>
          {favourites !== undefined && <th scope="col">Keep</th>}
        </tr>
      </thead>
      {sessions.map((session) => (
        <tbody key={session.session_id}>
          <tr>
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
            {favourites !== undefined && (
              <td>
                <FavouriteButton
                  favourites={favourites}
                  target={{ kind: 'session', sessionId: session.session_id }}
                  asRead={session.favourite}
                  of={`the ${session.game_name ?? 'unrecognised'} sitting from ${formatMoment(
                    session.started_at,
                  )}`}
                />
              </td>
            )}
          </tr>
          {actions !== undefined &&
            session.recordings.map((recording) => (
              <RecordingRow
                key={recording.recording_id}
                recording={recording}
                actions={actions}
                session={session}
                {...(favourites === undefined ? {} : { favourites })}
              />
            ))}
        </tbody>
      ))}
    </table>
  );
}

/** One recording of a sitting, and what can be done with it. */
function RecordingRow({
  recording,
  actions,
  session,
  favourites,
}: {
  readonly recording: LibraryRecording;
  readonly actions: RecordingActions;
  readonly session: LibrarySession;
  readonly favourites?: Favourites;
}): ReactNode {
  const available = canActOn(recording);
  const busy =
    actions.outcome.state === 'working' && actions.outcome.path === recording.path
      ? actions.outcome.what
      : undefined;
  /*
   * Every control names the file it acts on. Three buttons reading "Open" in a
   * table of forty recordings are three buttons a screen reader announces
   * identically, and a keyboard user cannot tell apart (AGENTS.md section 46).
   */
  const of = `${fileName(recording.path)}, ${session.game_name ?? 'not recognised'}`;
  const why = available
    ? undefined
    : 'This file could not be found the last time Clipped looked for it.';

  return (
    <tr>
      <td colSpan={4}>
        {fileName(recording.path)}
        {recording.duration_seconds !== undefined && (
          <span className="clipped-muted"> · {formatDuration(recording.duration_seconds)}</span>
        )}
        {recording.size_bytes !== undefined && available && (
          <span className="clipped-muted"> · {formatBytes(recording.size_bytes)}</span>
        )}
        {!available && <span className="clipped-muted"> · file missing</span>}
      </td>
      <td>
        <button
          type="button"
          disabled={!available || busy !== undefined}
          title={why}
          aria-label={`Open ${of}`}
          onClick={() => {
            actions.open(recording);
          }}
        >
          Open
        </button>{' '}
        <button
          type="button"
          disabled={!available || busy !== undefined}
          title={why}
          aria-label={`Show ${of} in Explorer`}
          onClick={() => {
            actions.reveal(recording);
          }}
        >
          Show in Explorer
        </button>{' '}
        <button
          type="button"
          disabled={!available || busy !== undefined}
          title={why}
          aria-label={`Export ${of} as MP4`}
          onClick={() => {
            actions.exportToMp4(recording);
          }}
        >
          {busy === 'Exporting' ? 'Exporting…' : 'Export MP4'}
        </button>
      </td>
      {favourites !== undefined && (
        <td>
          <FavouriteButton
            favourites={favourites}
            target={{ kind: 'recording', id: recording.recording_id }}
            asRead={recording.favourite}
            of={of}
          />
        </td>
      )}
    </tr>
  );
}

/**
 * The star, as a toggle rather than as two buttons.
 *
 * `aria-pressed` is what carries the state to a screen reader, and the glyph
 * changes shape as well as weight — a filled star against a hollow one — so the
 * mark survives being read in monochrome (AGENTS.md section 46). The label names
 * the thing rather than saying "Favourite", because a table of forty rows
 * otherwise announces forty identical buttons.
 */
function FavouriteButton({
  favourites,
  target,
  asRead,
  of,
}: {
  readonly favourites: Favourites;
  readonly target: FavouriteTarget;
  readonly asRead: boolean;
  readonly of: string;
}): ReactNode {
  const marked = favourites.isFavourite(target, asRead);
  const changing = favourites.isChanging(target);

  return (
    <button
      type="button"
      aria-pressed={marked}
      disabled={changing}
      aria-label={marked ? `Stop keeping ${of}` : `Keep ${of}`}
      onClick={() => {
        favourites.set(target, !marked);
      }}
    >
      <span aria-hidden="true">{marked ? '★' : '☆'}</span> {marked ? 'Kept' : 'Keep'}
    </button>
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
