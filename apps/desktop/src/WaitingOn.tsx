import type { ReactNode } from 'react';

/**
 * The table a screen draws instead of the one it cannot fill.
 *
 * Three screens now draw one — Games, Home and Library — and all three draw it
 * for the same reason. Each is specified against data this build cannot reach,
 * and an empty table with the deck's own column headings is indistinguishable
 * from a machine that has recorded nothing. The window has not looked; it
 * cannot. So what goes on the screen is what it owes and the work that supplies
 * it, one row each, naming the issue (AGENTS.md section 27).
 *
 * Shared rather than copied a third time (AGENTS.md section 55). The rows are
 * each screen's own; the shape and the second column's heading are not, because
 * the promise the table makes to the reader is the same on all three.
 */

/** One thing a screen will show, and the work that has to land before it can. */
export interface Waiting {
  /** What the screen will draw, in the reader's terms rather than the code's. */
  readonly shows: string;
  /** What has to exist before it can, ending in the issue that builds it. */
  readonly needs: string;
}

/** What a waiting-on table is given. */
export interface WaitingOnProps {
  /**
   * The first column's heading, which names the screen: "What the Library screen
   * will show".
   *
   * Per screen rather than fixed, because a reader who has skipped to the table
   * should not have to work out which screen it belongs to.
   */
  readonly heading: string;
  /** The rows, in the order the screen wants them read. */
  readonly rows: readonly Waiting[];
}

/** What a screen owes, against the work that lands each part of it. */
export function WaitingOn({ heading, rows }: WaitingOnProps): ReactNode {
  return (
    <table className="clipped-table">
      <thead>
        <tr>
          <th scope="col">{heading}</th>
          <th scope="col">What has to exist first</th>
        </tr>
      </thead>
      <tbody>
        {rows.map((row) => (
          <tr key={row.shows}>
            <td>{row.shows}</td>
            <td className="clipped-muted">{row.needs}</td>
          </tr>
        ))}
      </tbody>
    </table>
  );
}
