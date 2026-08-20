import type { HotkeyBinding } from '@clipped/shared';
import { type ReactNode, useState } from 'react';

import {
  UNBOUND,
  actionHolding,
  chordOf,
  conditionOf,
  describeCondition,
  describeHotkeyProblem,
  useHotkeyEditor,
  useHotkeys,
} from './hotkeys';

/**
 * Where every global hotkey stands, and how to change one, on the Settings
 * screen (issues #232 and #54).
 *
 * The window registers none of them. A combination another application owns
 * does not register, so the key does nothing, and until #232 nothing told the
 * user — the recorder discovered it at start-up and wrote a line to a log
 * nobody reads. This is where `Registration::conflicts()` is shown.
 *
 * # Why binding here works at all
 *
 * `apply_settings` writes the combination into the settings file *and* rebinds
 * the running service (`apps/recorder/src/hotkeys.rs`), which is issue #233 and
 * is closed. So this screen sends one save and the hotkey moves; it does not
 * ask anybody to restart anything, and #54's first criterion is a property of
 * the recorder rather than something arranged here.
 *
 * # Why "registered" is not the same as "works"
 *
 * Two questions, and a row that ran them together would be wrong half the time.
 * Windows can accept a combination for an action nothing in the build performs —
 * `Ctrl+F10` registers cleanly and no build saves a replay — so a row that read
 * the state alone would show a working hotkey for a key that reports itself as
 * unbuilt when pressed (AGENTS.md section 27). `conditionOf` is the answer to
 * both at once.
 *
 * It is also why every save is followed by a re-read. What comes back from
 * `apply_settings` is the setting; what this table draws is what Windows made
 * of it, and those differ exactly when the user most needs to know.
 */

/** What each condition is called, in the words a person reads. */
const CONDITION_LABEL: Readonly<Record<ReturnType<typeof conditionOf>, string>> = {
  working: 'Ready',
  unbound: 'Not bound',
  conflict: 'Taken',
  unavailable: 'Not in this build',
};

/** What the capture control says while it waits. */
const LISTENING = 'Press a combination, or Escape to stop';

/** One action's row. */
function Row({
  row,
  rows,
  pending,
  onBind,
}: {
  readonly row: HotkeyBinding;
  readonly rows: readonly HotkeyBinding[];
  readonly pending: string | null;
  readonly onBind: (action: string, chord: string | null) => void;
}): ReactNode {
  const [capturing, setCapturing] = useState(false);
  const [refused, setRefused] = useState<string | null>(null);
  const condition = conditionOf(row);
  const busy = pending === row.action;

  function capture(event: React.KeyboardEvent<HTMLButtonElement>): void {
    // Everything, including Tab: while this control is listening it is a key
    // catcher, and letting Tab through would move focus out mid-chord.
    event.preventDefault();

    if (event.key === 'Escape') {
      setCapturing(false);
      setRefused(null);
      return;
    }

    const chord = chordOf(event);
    if (chord === null) {
      // A modifier on its own, or a key this window cannot spell for the
      // recorder. Keep waiting rather than saving something nobody meant.
      return;
    }

    // The conflict this window can see. Saving it would trade one working
    // hotkey for another, and the user would find out by pressing the old one.
    const held = actionHolding(rows, chord, row.action);
    if (held !== null) {
      setRefused(`${chord} is already ${held.label}. Choose another combination.`);
      return;
    }

    setCapturing(false);
    setRefused(null);
    onBind(row.action, chord);
  }

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
      <td className="clipped-muted">{refused ?? describeCondition(row)}</td>
      <td>
        {/*
         * One button, which becomes the key catcher when it is pressed, rather
         * than a second control that appears already focused. Whatever put this
         * row into capture — a click or Enter — left focus here, so the chord
         * arrives without anybody being moved anywhere, and there is no
         * `autoFocus` to argue with (`jsx-a11y/no-autofocus`, which is right:
         * moving somebody's focus for them is the thing to avoid, and this
         * never does).
         */}
        <button
          type="button"
          className={
            capturing ? 'clipped-btn clipped-btn--primary' : 'clipped-btn clipped-btn--secondary'
          }
          disabled={busy}
          aria-pressed={capturing}
          onKeyDown={capturing ? capture : undefined}
          onBlur={() => {
            setCapturing(false);
            setRefused(null);
          }}
          onClick={() => {
            setRefused(null);
            setCapturing(true);
          }}
        >
          {capturing ? LISTENING : busy ? 'Saving…' : `Change ${row.label}`}
        </button>{' '}
        {capturing ? null : (
          <>
            {/*
             * Two controls, because they are two things. `UNBOUND` is the user
             * saying "nothing" and it survives the day the shipped default
             * changes; `null` stops the file saying anything, so the action
             * follows whatever Clipped ships with from then on. A single
             * "Clear" would have to pick one silently, and the two are a year
             * apart in consequence (`apps/recorder/src/settings.rs`).
             */}
            <button
              type="button"
              className="clipped-btn clipped-btn--ghost"
              disabled={busy || row.hotkey === UNBOUND}
              onClick={() => {
                onBind(row.action, UNBOUND);
              }}
            >
              {`Unbind ${row.label}`}
            </button>{' '}
            <button
              type="button"
              className="clipped-btn clipped-btn--ghost"
              disabled={busy}
              onClick={() => {
                onBind(row.action, null);
              }}
            >
              {`Reset ${row.label}`}
            </button>
          </>
        )}
      </td>
    </tr>
  );
}

/** The hotkeys the recorder registered, or why they are not known. */
export function HotkeyList(): ReactNode {
  const read = useHotkeys();
  const editor = useHotkeyEditor(read.state === 'read' ? read.value : null);

  return (
    <section className="clipped-panel" aria-label="Global hotkeys">
      <h3 className="clipped-panel__heading">What the recorder registered</h3>
      <p className="clipped-panel__body clipped-muted">
        The recorder registers these, not this window: Windows gives a combination to one process,
        and the recorder is the one that keeps running when this window is closed. A change here
        reaches it straight away.
      </p>

      {read.state === 'reading' ? (
        <p className="clipped-panel__body">Asking the recorder…</p>
      ) : null}

      {read.state === 'unread' ? (
        <p className="clipped-panel__body">{describeHotkeyProblem(read.problem)}</p>
      ) : null}

      {editor.problem !== null ? (
        <p className="clipped-panel__body" role="alert">
          {describeHotkeyProblem(editor.problem)}
        </p>
      ) : null}

      {editor.rows !== null ? (
        <table className="clipped-table">
          <thead>
            <tr>
              <th scope="col">Action</th>
              <th scope="col">Combination</th>
              <th scope="col">State</th>
              <th scope="col">What that means</th>
              <th scope="col">Change</th>
            </tr>
          </thead>
          <tbody>
            {editor.rows.map((row) => (
              <Row
                key={row.action}
                row={row}
                rows={editor.rows ?? []}
                pending={editor.pending}
                onBind={(action, chord) => {
                  void editor.bind(action, chord);
                }}
              />
            ))}
          </tbody>
        </table>
      ) : null}
    </section>
  );
}
