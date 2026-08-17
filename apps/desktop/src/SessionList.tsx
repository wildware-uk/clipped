import type { LibraryRecording, LibrarySession } from '@clipped/shared';
import { useRef, type ReactNode } from 'react';

import {
  footageSeconds,
  formatBytes,
  formatDuration,
  formatMoment,
  missingCount,
  presentBytes,
} from './library';
import type { Favourites, FavouriteTarget } from './favourites';
import type { Locks, LockTarget } from './locks';
import { canActOn, fileName, type RecordingActions } from './recordingActions';
import { useSessionWindow } from './virtualWindow';

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
 * A recording whose file has gone gets the same controls **disabled**,
 * rather than hidden: hiding them would leave a row with nothing on it and no
 * explanation, and a disabled control that says why is what AGENTS.md sections
 * 27 and 45 ask for.
 *
 * # Export asks the recorder as well as the row
 *
 * The other two controls are shell calls this window's own host makes. Export
 * is a command the *recorder* performs, and a recorder built before issue #399
 * does not have it — so the control asks the features in that recorder's
 * welcome before it draws itself, and is disabled with the reason in its label
 * when they do not name `export` (issue #447). Without the check the refusal
 * arrives after the Save As dialog, which is the only part of the interaction
 * that costs the user anything.
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
 *
 * # Large libraries scroll smoothly
 *
 * Issue #60's second acceptance criterion. `useSessionWindow` (`virtualWindow.ts`)
 * trims which sessions are actually mounted to what the measured shell viewport
 * needs, plus a small overscan either side, with two spacer rows standing in for
 * whatever was skipped so the scrollbar keeps roughly the right size. It is a
 * no-op — every session renders, exactly as before — until there is a real
 * `.clipped-shell__main` to measure, which is why every case in
 * `LibraryScreen.test.tsx` and `HomeScreen.test.tsx` still sees every session it
 * always saw: jsdom has no layout engine and reports a viewport of zero.
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
   * Opens a recording on the playback screen.
   *
   * Separate from {@link SessionListProps.actions} because it is not a round
   * trip to anywhere: it is navigation, and the row it hands over is what saves
   * the playback screen a second read of what this list already has (issue
   * #304). Absent draws no Play control, which is a control that would do
   * nothing.
   */
  readonly onPlay?: (recording: LibraryRecording) => void;
  /**
   * Marking a sitting or a recording as one to keep (issue #58).
   *
   * Absent draws no stars at all. Home passes none for the same reason it
   * passes no actions: it is a summary of what has been recorded lately, and
   * the Library is where a sitting is acted on.
   */
  readonly favourites?: Favourites;
  /**
   * Keeping a sitting or a recording out of automatic cleanup's reach (issue
   * #472).
   *
   * Absent draws no padlocks. Separate from {@link favourites} because they
   * are separate statements: a favourite says this one was good, a lock says
   * do not reclaim this space. Both protect against cleanup, and only one of
   * them is *about* that.
   */
  readonly locks?: Locks;
}

/** The sittings, one row each, and their recordings under them. */
export function SessionList({
  sessions,
  label,
  actions,
  favourites,
  locks,
  onPlay,
}: SessionListProps): ReactNode {
  const table = useRef<HTMLTableElement>(null);
  const showsRecordings = actions !== undefined;
  const window_ = useSessionWindow(table, sessions, showsRecordings);
  const visible = sessions.slice(window_.start, window_.end);

  return (
    <table className="clipped-table" aria-label={label} ref={table}>
      <thead>
        <tr>
          <th scope="col">Game</th>
          <th scope="col">Recorded</th>
          <th scope="col">Footage</th>
          <th scope="col">Size</th>
          <th scope="col">Files</th>
          {favourites !== undefined && <th scope="col">Keep</th>}
          {locks !== undefined && <th scope="col">Cleanup</th>}
        </tr>
      </thead>
      {/*
       * Stands in for the sessions skipped above `window_.start`, so the
       * scrollbar reads roughly the length a fully-mounted table would have
       * had. `aria-hidden` keeps it out of the accessibility tree entirely —
       * an empty row would otherwise be a row nobody can make sense of, and
       * every row-index assertion in the existing suites counts real sessions
       * only.
       */}
      {window_.topSpacerPx > 0 && (
        <tbody aria-hidden="true">
          <tr style={{ height: `${String(window_.topSpacerPx)}px` }}>
            <td colSpan={5} />
          </tr>
        </tbody>
      )}
      {visible.map((session) => (
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
            {locks !== undefined && (
              <td>
                <LockButton
                  locks={locks}
                  target={{ kind: 'session', sessionId: session.session_id }}
                  asRead={session.locked ?? false}
                  protectedNow={session.locked ?? false}
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
                onPlay={onPlay}
                session={session}
                {...(favourites === undefined ? {} : { favourites })}
                {...(locks === undefined ? {} : { locks })}
              />
            ))}
        </tbody>
      ))}
      {window_.bottomSpacerPx > 0 && (
        <tbody aria-hidden="true">
          <tr style={{ height: `${String(window_.bottomSpacerPx)}px` }}>
            <td colSpan={5} />
          </tr>
        </tbody>
      )}
    </table>
  );
}

/** One recording of a sitting, and what can be done with it. */
function RecordingRow({
  recording,
  actions,
  onPlay,
  session,
  favourites,
  locks,
}: {
  readonly recording: LibraryRecording;
  readonly actions: RecordingActions;
  // Not optional but possibly absent, which is what `exactOptionalPropertyTypes`
  // asks a caller to be explicit about: this component is internal and is always
  // handed the list's own answer, whether or not there is one.
  readonly onPlay: ((recording: LibraryRecording) => void) | undefined;
  readonly session: LibrarySession;
  readonly favourites?: Favourites;
  readonly locks?: Locks;
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
        {onPlay !== undefined && (
          <>
            {/*
             * First, because watching it is what most people came for, and it
             * is the one action that happens *here* rather than in another
             * application (issue #304).
             */}
            <button
              type="button"
              disabled={!available || busy !== undefined}
              title={why}
              aria-label={`Play ${of}`}
              onClick={() => {
                onPlay(recording);
              }}
            >
              Play
            </button>{' '}
          </>
        )}
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
        <ExportButton
          actions={actions}
          recording={recording}
          of={of}
          available={available}
          busy={busy}
          whyNot={why}
        />
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
      {locks !== undefined && (
        <td>
          <LockButton
            locks={locks}
            target={{ kind: 'recording', id: recording.recording_id }}
            asRead={recording.locked ?? false}
            protectedNow={recording.protected ?? false}
            of={of}
          />
        </td>
      )}
    </tr>
  );
}

/**
 * The Export control, which asks the recorder before it offers itself.
 *
 * Two questions, in this order, because they are about different things and
 * send somebody to different places:
 *
 * - **can this recorder export at all**, from the features in its welcome
 *   (`recordingActions.ts`). An older recorder has no `export_recording`
 *   command, and finding that out costs a Save As dialog and a file name for a
 *   file that was never going to be written
 *   ([issue #447](https://github.com/wildware-uk/clipped/issues/447)). The way
 *   out is restarting Clipped;
 * - **is this recording's file still there**, which is the row's own question
 *   and the one the other three controls share. The way out is finding the
 *   file.
 *
 * Disabled with the reason in its own label rather than hidden, which is what
 * the tray's menu does with the same question and what AGENTS.md sections 27
 * and 45 ask for: a control that vanishes explains nothing, and a control that
 * is greyed out with no reason explains less. The label carries the short form
 * and the accessible name carries the sentence, because `aria-label` is what a
 * screen reader announces instead of the text, and a `title` is not announced
 * at all.
 */
function ExportButton({
  actions,
  recording,
  of,
  available,
  busy,
  whyNot,
}: {
  readonly actions: RecordingActions;
  readonly recording: LibraryRecording;
  readonly of: string;
  readonly available: boolean;
  readonly busy: string | undefined;
  readonly whyNot: string | undefined;
}): ReactNode {
  const offer = actions.canExport;

  if (!offer.offered) {
    return (
      <button
        type="button"
        disabled
        title={offer.why}
        aria-label={`Export ${of} as MP4 — ${offer.why}`}
      >
        Export MP4 — {offer.shortly}
      </button>
    );
  }

  return (
    <button
      type="button"
      disabled={!available || busy !== undefined}
      title={whyNot}
      aria-label={`Export ${of} as MP4`}
      onClick={() => {
        actions.exportToMp4(recording);
      }}
    >
      {busy === 'Exporting' ? 'Exporting…' : 'Export MP4'}
    </button>
  );
}

/**
 * The padlock, which says two things at once.
 *
 * A recording inside a locked sitting is protected and has no lock of its own,
 * so the control has to distinguish "you locked this" from "cleanup will not
 * take this". It does that in words rather than by shade: the button is
 * disabled and reads "Kept by sitting", because there is nothing on this row to
 * release and offering a control that would do nothing is worse than not
 * offering one (AGENTS.md sections 27 and 45).
 *
 * The wording avoids "Locked" on its own, which would read as "you cannot
 * delete this". A lock stops automatic cleanup and nothing else.
 */
function LockButton({
  locks,
  target,
  asRead,
  protectedNow,
  of,
}: {
  readonly locks: Locks;
  readonly target: LockTarget;
  readonly asRead: boolean;
  readonly protectedNow: boolean;
  readonly of: string;
}): ReactNode {
  const locked = locks.isLocked(target, asRead);
  const changing = locks.isChanging(target);
  // Protected without a lock of its own: the sitting's lock is doing it, and
  // this row has nothing to release.
  const bySitting = !locked && protectedNow;

  if (bySitting) {
    return (
      <button
        type="button"
        disabled
        title="Its sitting is protected, so automatic cleanup will not take this. Change it on the sitting."
        aria-label={`${of} is protected from automatic cleanup because its sitting is`}
      >
        <span aria-hidden="true">🔒</span> By sitting
      </button>
    );
  }

  return (
    <button
      type="button"
      aria-pressed={locked}
      disabled={changing}
      title="Automatic cleanup deletes the oldest recordings when a storage limit is reached. This keeps one out of that. Deleting it yourself still works."
      aria-label={
        locked
          ? `Stop protecting ${of} from automatic cleanup`
          : `Protect ${of} from automatic cleanup`
      }
      onClick={() => {
        locks.set(target, !locked);
      }}
    >
      <span aria-hidden="true">{locked ? '🔒' : '🔓'}</span> {locked ? 'Protected' : 'Protect'}
    </button>
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
