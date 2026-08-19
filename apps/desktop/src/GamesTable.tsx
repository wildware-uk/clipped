import type { LibraryGame } from '@clipped/shared';
import type { ReactNode } from 'react';

import { formatBytes, formatMoment } from './library';

/**
 * What the library holds per game, drawn the same way wherever it is shown.
 *
 * Home and the Games screen both answer "what has been recorded, of what", and
 * a second copy of this table would be two places for "the row for games the
 * catalogue would not attribute" to be got wrong (AGENTS.md section 55). The
 * same reason `SessionList` is shared between Home and the Library.
 *
 * # Two widths, named rather than flagged
 *
 * Home is a summary — what has been recorded lately, above a list of sittings —
 * and a table with seven columns there buries it. The Games screen is
 * SPEC.md section 17's table and is the place those columns belong. So the
 * caller says which it wants by name rather than passing a boolean nobody can
 * read at the call site.
 *
 * # The unattributed row
 *
 * `game_id` and `name` are both absent on the row for sittings the catalogue
 * would not attribute, and there is at most one, last. It is drawn as **Not
 * recognised** rather than hidden: those sittings are recordings somebody made,
 * and a table that quietly omitted them would under-report what is on the disk.
 */
export interface GamesTableProps {
  /** The rows, in the order the library returned them. */
  readonly games: readonly LibraryGame[];
  /**
   * How much of each row to draw.
   *
   * `summary` is what Home has always shown. `everything` adds the columns
   * SPEC.md section 17 asks for and Home leaves out.
   */
  readonly showing: 'summary' | 'everything';
  /** What to call the table, for a screen reader. */
  readonly label: string;
}

/** The per-game table. */
export function GamesTable({ games, showing, label }: GamesTableProps): ReactNode {
  const everything = showing === 'everything';

  return (
    <table className="clipped-table" aria-label={label}>
      <thead>
        <tr>
          <th scope="col">Game</th>
          <th scope="col">Sessions</th>
          <th scope="col">Recordings</th>
          {everything && <th scope="col">Clips</th>}
          {everything && <th scope="col">Kept</th>}
          <th scope="col">Size</th>
          <th scope="col">Files</th>
          {everything && <th scope="col">Last played</th>}
        </tr>
      </thead>
      <tbody>
        {games.map((game) => (
          <tr key={game.game_id ?? 'unattributed'}>
            <td>{game.name ?? 'Not recognised'}</td>
            <td>{game.sessions}</td>
            <td>{game.recordings}</td>
            {everything && <td>{game.clips}</td>}
            {everything && <td>{game.favourites}</td>}
            {/*
             * A missing file contributes nothing to the size — the space it is
             * not occupying is not being used — and is counted beside it
             * instead, in words (docs/library.md).
             */}
            <td>{formatBytes(game.bytes)}</td>
            <td>{game.missing === 0 ? 'All present' : `${String(game.missing)} missing`}</td>
            {everything && (
              <td>
                {/*
                 * Absent rather than "never": a game with sittings against it
                 * has been played, and a library index that has not recorded
                 * when is a different thing from a game nobody has opened. Said
                 * as "not recorded" so nobody reads a gap as a zero.
                 */}
                {game.last_played_at === undefined
                  ? 'not recorded'
                  : formatMoment(game.last_played_at)}
              </td>
            )}
          </tr>
        ))}
      </tbody>
    </table>
  );
}
