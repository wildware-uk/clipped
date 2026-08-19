import type { RecordingList, StorageReport } from '@clipped/shared';
import type { ReactNode } from 'react';
import { useEffect } from 'react';

import { describeProblem } from './library';
import { size, useStorage } from './storage';

/**
 * What the library occupies, drawn from the recorder's own measurement
 * (SPEC.md section 27, issue #95).
 *
 * # Every figure here has a producer
 *
 * One command answers all of it — `get_storage`, which the recorder serves from
 * `clipped_session::cleanup::preview`, the same measurement its storage sweep
 * takes before it deletes anything. So the usage is a walk of the recording and
 * trash folders, the free space is what the volume reported, the deletions are
 * the plan a sweep would carry out, and what is protected is the rules that
 * would stop it. Nothing on this panel is derived from anything else, and
 * nothing is a placeholder.
 *
 * What it does *not* draw is as deliberate. There is no per-game breakdown here:
 * `library_games` carries that and the Home screen draws it, and a second answer
 * from a second measurement is two opinions about one question (AGENTS.md
 * section 55). There is no expiry date against anything in the trash, because
 * nothing configures a retention period and a date computed from a policy nobody
 * set would be a promise the application cannot keep.
 *
 * # Four states, not one
 *
 * Reading, unreadable, read-and-empty and read-with-something-in-it, for the
 * reason `HomeScreen.tsx` draws four: a panel of zeroes over a measurement that
 * failed is indistinguishable from a machine with nothing on it, and the two
 * would send somebody in opposite directions (AGENTS.md section 27).
 */

/** What one bar means, in words, for anybody not reading the bar. */
function Used({ report }: { readonly report: StorageReport }): ReactNode {
  const used = report.capacity_bytes - report.free_bytes;
  const fraction = report.capacity_bytes > 0 ? used / report.capacity_bytes : undefined;

  return (
    <>
      <p className="clipped-panel__body">
        Clipped is using <strong>{size(report.usage_bytes)}</strong>. The drive has{' '}
        <strong>{size(report.free_bytes)}</strong> free of {size(report.capacity_bytes)}.
      </p>
      {/*
       * A `<meter>` of the *drive*, not of Clipped: the disk holds other
       * applications' files too, and a bar that showed only Clipped's share
       * would say a drive was nearly empty while it filled. With no value at
       * all where the volume reported no capacity, for the reason `ExportBar`
       * has none where a recording never said how long it was — a value of
       * nought is a claim.
       */}
      <meter
        aria-label="How full the drive is"
        min={0}
        max={1}
        value={fraction ?? undefined}
        high={0.9}
      />
    </>
  );
}

/** What each kind of file occupies. */
function Breakdown({ report }: { readonly report: StorageReport }): ReactNode {
  if (report.by_category.length === 0) {
    return (
      <p className="clipped-panel__body">
        Nothing was found in the recording folder or the trash, so there is nothing to break down.
      </p>
    );
  }

  return (
    <table className="clipped-table" aria-label="What Clipped is using">
      <thead>
        <tr>
          <th scope="col">What</th>
          <th scope="col">Size</th>
        </tr>
      </thead>
      <tbody>
        {report.by_category.map((usage) => (
          <tr key={usage.category}>
            <th scope="row">{usage.category}</th>
            <td>{size(usage.bytes)}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

/**
 * The rules that keep recordings out of a sweep, as counted state.
 *
 * SPEC.md section 27 promises that favourites and locked footage are never
 * deleted automatically. This is that promise with figures against it, which is
 * the difference between a claim and something a user can check (AGENTS.md
 * section 27). Each label is the recorder's own — the vocabulary of protections
 * lives with the code that applies them.
 */
function Protected({ report }: { readonly report: StorageReport }): ReactNode {
  if (report.protected.length === 0) {
    return (
      <p className="clipped-panel__body">
        Nothing is protected from automatic cleanup yet. Favouriting a recording or locking one
        keeps it, and a sitting’s star or padlock keeps everything in it.
      </p>
    );
  }

  return (
    <table className="clipped-table" aria-label="Never deleted automatically">
      <thead>
        <tr>
          <th scope="col">Kept because</th>
          <th scope="col">Recordings</th>
          <th scope="col">Size</th>
        </tr>
      </thead>
      <tbody>
        {report.protected.map((group) => (
          <tr key={group.label}>
            <th scope="row">{group.label}</th>
            <td>{String(group.recordings)}</td>
            <td>{size(group.bytes)}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}

/** The name at the end of a path, which is what a row can be recognised by. */
function fileName(path: string): string {
  return path.split(/[\\/]/).pop() ?? path;
}

/**
 * The recordings filling the drive, largest first.
 *
 * The review path SPEC.md section 27 and issue #111 ask for: somebody who can
 * see what is taking the room can act on it before automatic cleanup does. Every
 * row says whether a sweep may take it, because "the biggest thing here is
 * protected" is the answer that explains a disk staying full.
 */
function Largest({ listed }: { readonly listed: RecordingList }): ReactNode {
  if (listed.total === 0) {
    return (
      <p className="clipped-panel__body">
        The library index holds no recordings, so there is nothing to review.
      </p>
    );
  }

  return (
    <>
      <table className="clipped-table" aria-label="Largest recordings">
        <thead>
          <tr>
            <th scope="col">Recording</th>
            <th scope="col">Size</th>
            <th scope="col">Recorded</th>
            <th scope="col">Automatic cleanup</th>
          </tr>
        </thead>
        <tbody>
          {listed.recordings.map((recording) => (
            <tr key={recording.recording_id}>
              <th scope="row">
                <span className="clipped-path">{fileName(recording.path)}</span>
              </th>
              <td>{size(recording.size_bytes)}</td>
              <td>{recording.started_at.slice(0, 10)}</td>
              <td className="clipped-muted">
                {recording.protected_because === undefined
                  ? 'may take it'
                  : `will not take it: ${recording.protected_because}`}
              </td>
            </tr>
          ))}
        </tbody>
      </table>
      {listed.recordings.length < listed.total ? (
        <p className="clipped-muted">
          The {String(listed.recordings.length)} largest of {String(listed.total)} recordings,{' '}
          {size(listed.total_bytes)} in all.
        </p>
      ) : null}
    </>
  );
}

/**
 * What automatic cleanup would do right now, under the limits that are saved.
 *
 * Not a warning about something that might happen later: the sweep runs after
 * every reconciliation, so a non-empty plan is footage on its way to the trash.
 * Drawn as what it is, with the way back — everything it takes goes to the trash
 * first and can be restored from the Library screen (SPEC.md section 28).
 */
function Sweep({ report }: { readonly report: StorageReport }): ReactNode {
  if (
    report.limits.maximum_usage_bytes === undefined &&
    report.limits.minimum_free_space_bytes === undefined &&
    report.limits.maximum_age_days === undefined
  ) {
    return (
      <p className="clipped-panel__body">
        No limit is set, so Clipped deletes nothing on its own. Everything above stays until you
        delete it.
      </p>
    );
  }

  if (report.would_delete.total === 0) {
    return (
      <p className="clipped-panel__body">
        The library is inside its limits, so automatic cleanup would take nothing.
      </p>
    );
  }

  return (
    <p className="clipped-panel__body" role="status">
      Automatic cleanup would move {String(report.would_delete.total)} recording(s),{' '}
      {size(report.would_delete.total_bytes)}, to the trash — the oldest first, and nothing that is
      protected. They can be restored from the Library screen until the trash is emptied.
      {report.still_over_limit > 0
        ? ` Even then it would be ${size(report.still_over_limit)} over, because everything else is protected.`
        : ''}
    </p>
  );
}

/**
 * The measured half of the Storage section.
 *
 * `refreshOn` is bumped by the screen after a setting is saved, so the figures
 * follow the limit that was just applied rather than describing the one before
 * it. Not a timer: a measurement is a directory walk at the other end, and a
 * panel that repeated it every few seconds would keep a disk busy for a figure
 * that changes when a recording ends.
 */
export function StorageAccount({ refreshOn }: { readonly refreshOn: number }): ReactNode {
  const storage = useStorage();
  const { again } = storage;

  useEffect(() => {
    if (refreshOn > 0) {
      again();
    }
  }, [refreshOn, again]);

  return (
    <section className="clipped-panel" aria-label="What Clipped is using">
      <h3 className="clipped-panel__heading">What Clipped is using</h3>

      {storage.read.state === 'reading' ? (
        <p className="clipped-panel__body" aria-busy="true">
          Measuring your library…
        </p>
      ) : null}

      {/*
       * Said, never drawn as zeroes. Every figure this panel could invent from
       * a failed measurement is one somebody would act on, and the two obvious
       * inventions point in opposite directions: "nothing would be deleted" is
       * what a limit gets set on the strength of, and "no free space" is what
       * recordings get deleted on the strength of (AGENTS.md sections 27 and 56).
       */}
      {storage.read.state === 'unread' ? (
        <p className="clipped-panel__body" role="status">
          {describeProblem(storage.read.problem)}
        </p>
      ) : null}

      {storage.read.state === 'read' ? (
        <>
          <p className="clipped-panel__body">
            Measured in{' '}
            <code className="clipped-code">{storage.read.value.recordings_directory}</code>, which
            is where the recorder is writing now, and in its trash at{' '}
            <code className="clipped-code">{storage.read.value.trash_directory}</code>.
          </p>

          {/*
           * The sentence issue #95's second criterion is about, and it is here
           * rather than only beside the field because it is true whether or not
           * anybody is changing the folder today. Recordings are ordinary files
           * and the library indexes where each one is (AGENTS.md section 32), so
           * moving the setting moves nothing: what is already recorded stays
           * where it is, keeps playing, and keeps its place in the library. What
           * changes is where the next sitting is written — and only the folder
           * in use is measured above, so a library spread over two folders reads
           * as smaller here than it is (issue #272).
           */}
          <p className="clipped-muted">
            Changing the recording folder moves nothing. Recordings already made stay where they
            are, still play, and stay in your library; only new sittings go to the new folder. The
            figures above are of the folder in use.
          </p>

          <Used report={storage.read.value} />
          <Breakdown report={storage.read.value} />

          <h3 className="clipped-panel__heading">What automatic cleanup would do</h3>
          <Sweep report={storage.read.value} />

          <h3 className="clipped-panel__heading">Never deleted automatically</h3>
          <Protected report={storage.read.value} />

          <h3 className="clipped-panel__heading">Largest recordings</h3>
          <Largest listed={storage.read.value.largest} />
        </>
      ) : null}
    </section>
  );
}
