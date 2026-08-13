import type { HotkeyBinding } from '@clipped/shared';
import type { ReactNode } from 'react';

import { conditionOf, describeCondition, describeHotkeyProblem, useHotkeys } from './hotkeys';

/**
 * Where every global hotkey stands, on the Settings screen (issue #232).
 *
 * The one thing on this screen that is not an account of something the window
 * cannot see, and it is here because of what a hotkey fails like. A combination
 * another application owns does not register, so the key does nothing, and
 * nothing tells the user — the recorder discovers it at start-up and writes a
 * line to a log nobody reads. `Registration::conflicts()` has existed since
 * issue #39 and had nowhere to be shown; this is where.
 *
 * Nothing here is editable. Binding a combination from this window is
 * [issue #54](https://github.com/wildware-uk/clipped/issues/54), and doing it
 * without restarting the recorder is
 * [issue #233](https://github.com/wildware-uk/clipped/issues/233); until then
 * the rest of this screen's rule holds, and what is drawn is the state and how
 * to change it.
 *
 * # Why "registered" is not the same as "works"
 *
 * Two questions, and a row that ran them together would be wrong half the time.
 * Windows can accept a combination for an action nothing in the build performs —
 * `Ctrl+F10` registers cleanly and no build saves a replay — so a row that read
 * the state alone would show a working hotkey for a key that reports itself as
 * unbuilt when pressed (AGENTS.md section 27). `conditionOf` is the answer to
 * both at once.
 */

/** What each condition is called, in the words a person reads. */
const CONDITION_LABEL: Readonly<Record<ReturnType<typeof conditionOf>, string>> = {
  working: 'Ready',
  unbound: 'Not bound',
  conflict: 'Taken',
  unavailable: 'Not in this build',
};

/** One action's row. */
function Row({ row }: { readonly row: HotkeyBinding }): ReactNode {
  const condition = conditionOf(row);

  return (
    <tr>
      <th scope="row">{row.label}</th>
      <td>
        {row.hotkey === undefined ? (
          <span className="clipped-muted">—</span>
        ) : (
          <code className="clipped-code">{row.hotkey}</code>
        )}
      </td>
      {/*
       * The word and the sentence, not a colour: the state has to be legible to
       * somebody who cannot tell one tag from another (AGENTS.md section 46).
       */}
      <td>
        <span className="clipped-tag clipped-tag--outline">{CONDITION_LABEL[condition]}</span>
      </td>
      <td className="clipped-muted">{describeCondition(row)}</td>
    </tr>
  );
}

/** The hotkeys the recorder registered, or why they are not known. */
export function HotkeyList(): ReactNode {
  const read = useHotkeys();

  return (
    <section className="clipped-panel" aria-label="Global hotkeys">
      <h3 className="clipped-panel__heading">What the recorder registered</h3>
      <p className="clipped-panel__body clipped-muted">
        The recorder registers these, not this window: Windows gives a combination to one process,
        and the recorder is the one that keeps running when this window is closed.
      </p>

      {read.state === 'reading' ? (
        <p className="clipped-panel__body">Asking the recorder…</p>
      ) : null}

      {read.state === 'unread' ? (
        <p className="clipped-panel__body">{describeHotkeyProblem(read.problem)}</p>
      ) : null}

      {read.state === 'read' ? (
        <table className="clipped-table">
          <thead>
            <tr>
              <th scope="col">Action</th>
              <th scope="col">Combination</th>
              <th scope="col">State</th>
              <th scope="col">What that means</th>
            </tr>
          </thead>
          <tbody>
            {read.value.map((row) => (
              <Row key={row.action} row={row} />
            ))}
          </tbody>
        </table>
      ) : null}
    </section>
  );
}
