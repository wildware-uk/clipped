/**
 * The catalogue the recorder knows, and the controls that change it.
 *
 * SPEC.md section 6 asks for four things a person can do to the catalogue:
 * register an executable Clipped does not recognise, rename a game, exclude one
 * and drop one they added. `clipped-game-detection` has been able to do all of
 * them since #45 and nothing called any of them, so this screen drew a table and
 * no controls at all
 * ([issue #245](https://github.com/wildware-uk/clipped/issues/245)).
 *
 * # Nothing here is drawn unless the recorder says it works
 *
 * Every control is behind `catalogue_editing`, which is a separate capability
 * from `catalogue`: a recorder that can list games cannot necessarily change
 * them, and every build between #245's two halves could do exactly the first.
 * A button whose command would be refused may not be drawn (AGENTS.md
 * section 27).
 */

import type { CatalogueGame } from '@clipped/shared';
import { useState, type FormEvent, type ReactNode } from 'react';

import {
  forgetGame,
  registerGame,
  renameGame,
  setGameExcluded,
  type CatalogueEditor,
} from './catalogueEdits';
import { describeProblem } from './library';

/** What {@link CatalogueGames} needs. */
export interface CatalogueGamesProps {
  /** The catalogue and the edits, from `useCatalogueEditor`. */
  readonly editor: CatalogueEditor;
  /**
   * Whether this recorder can change the catalogue.
   *
   * When false the table is drawn and the controls are not: listing and
   * changing are separate capabilities, and a recorder that can only list is a
   * normal thing to be attached to rather than a fault.
   */
  readonly canEdit: boolean;
}

/** The catalogue table, with the controls when the recorder has them. */
export function CatalogueGames({ editor, canEdit }: CatalogueGamesProps): ReactNode {
  const games = editor.games ?? [];
  const [renaming, setRenaming] = useState<string | null>(null);

  return (
    <>
      {canEdit && <RegisterGameForm editor={editor} />}

      {/*
       * A live region, because an edit's outcome is the whole point of pressing
       * the button and the table it changes is long enough to scroll away from.
       */}
      {editor.problem !== null && (
        <p className="clipped-panel__body" role="alert">
          {describeProblem(editor.problem)}
        </p>
      )}

      <table className="clipped-table" aria-label="Games Clipped knows">
        <thead>
          <tr>
            <th scope="col">Game</th>
            <th scope="col">Recognised by</th>
            <th scope="col">Launcher</th>
            <th scope="col">Entry</th>
            {canEdit && <th scope="col">Change</th>}
          </tr>
        </thead>
        <tbody>
          {games.map((game) => (
            <tr key={game.game_id}>
              <td>
                {renaming === game.game_id ? (
                  <RenameField
                    game={game}
                    editor={editor}
                    onFinished={(): void => {
                      setRenaming(null);
                    }}
                  />
                ) : (
                  <>
                    {game.name}
                    {/*
                     * In words rather than by colour or by leaving the row out.
                     * An exclusion is a decision about an entry rather than the
                     * deletion of one, and somebody who excluded a game has to
                     * be able to find it again (AGENTS.md section 46).
                     */}
                    {game.excluded && <span className="clipped-muted"> · excluded</span>}
                  </>
                )}
              </td>
              <td>{game.executables.map((rule) => rule.name).join(', ')}</td>
              <td>
                {game.launcher === undefined ? (
                  <span className="clipped-muted">by name only</span>
                ) : (
                  game.launcher
                )}
              </td>
              <td className="clipped-muted">
                {game.source === 'user' ? 'yours' : 'shipped with Clipped'}
              </td>
              {canEdit && (
                <td>
                  <RowControls
                    game={game}
                    editor={editor}
                    renaming={renaming === game.game_id}
                    onRename={(): void => {
                      setRenaming(game.game_id);
                    }}
                  />
                </td>
              )}
            </tr>
          ))}
        </tbody>
      </table>
    </>
  );
}

/** The buttons on one row. */
function RowControls({
  game,
  editor,
  renaming,
  onRename,
}: {
  readonly game: CatalogueGame;
  readonly editor: CatalogueEditor;
  readonly renaming: boolean;
  readonly onRename: () => void;
}): ReactNode {
  // Only this row's controls, rather than every row's, because only this row is
  // changing. Disabling the table because one entry is busy would make a slow
  // recorder look like a broken screen.
  const busy = editor.pending === game.game_id;

  return (
    <span className="clipped-field">
      <button
        type="button"
        className="clipped-btn clipped-btn--secondary"
        disabled={busy || renaming}
        onClick={(): void => {
          onRename();
        }}
      >
        Rename
      </button>
      <button
        type="button"
        className="clipped-btn clipped-btn--secondary"
        disabled={busy}
        onClick={(): void => {
          void editor.apply(game.game_id, () => setGameExcluded(game.game_id, !game.excluded));
        }}
      >
        {/*
         * The verb for what pressing it does, not the state it is in. "Excluded"
         * on a button is ambiguous about which way it points, and this control
         * is the one that decides whether a game is recorded at all.
         */}
        {game.excluded ? 'Include' : 'Exclude'}
      </button>
      {/*
       * Only for entries the user added. Forgetting one Clipped ships would be
       * undone by the next update, which is why excluding is the operation that
       * lasts — and offering a button that does not last would be a worse
       * answer than not offering it.
       */}
      {game.source === 'user' && (
        <button
          type="button"
          className="clipped-btn clipped-btn--secondary"
          disabled={busy}
          onClick={(): void => {
            void editor.apply(game.game_id, () => forgetGame(game.game_id));
          }}
        >
          Forget
        </button>
      )}
    </span>
  );
}

/** The name field, while a row is being renamed. */
function RenameField({
  game,
  editor,
  onFinished,
}: {
  readonly game: CatalogueGame;
  readonly editor: CatalogueEditor;
  readonly onFinished: () => void;
}): ReactNode {
  const [name, setName] = useState(game.name);
  const busy = editor.pending === game.game_id;

  return (
    <form
      className="clipped-field"
      onSubmit={(event: FormEvent): void => {
        event.preventDefault();
        // An empty box is the clearing form rather than an error: somebody who
        // selects the name and deletes it means "use the one it shipped with",
        // which is a thing the protocol expresses.
        const wanted = name.trim();
        void editor
          .apply(game.game_id, () => renameGame(game.game_id, wanted === '' ? null : wanted))
          .then(onFinished);
      }}
    >
      <input
        className="clipped-input"
        type="text"
        // Rather than a hidden label: there is no visually-hidden helper in
        // `packages/ui`, and a visible "Name for X" in a table cell is clutter.
        // The name is in it because a row's input is otherwise unidentifiable
        // in a table of eighty.
        aria-label={`Name for ${game.name}`}
        value={name}
        disabled={busy}
        onChange={(event): void => {
          setName(event.target.value);
        }}
      />
      <button type="submit" className="clipped-btn clipped-btn--primary" disabled={busy}>
        Save
      </button>
      <button
        type="button"
        className="clipped-btn clipped-btn--secondary"
        disabled={busy}
        onClick={onFinished}
      >
        Cancel
      </button>
      {game.source === 'shipped' && (
        <span className="clipped-muted">
          Clearing this puts back the name Clipped ships, and an update may still correct how the
          game is recognised.
        </span>
      )}
    </form>
  );
}

/** Registering a game the catalogue does not know. */
function RegisterGameForm({ editor }: { readonly editor: CatalogueEditor }): ReactNode {
  const [name, setName] = useState('');
  const [executable, setExecutable] = useState('');
  const [fragment, setFragment] = useState('');
  // The identifier is the recorder's to choose, so a registration in flight is
  // named by something no entry can be called.
  const busy = editor.pending === REGISTERING;

  const ready = name.trim() !== '' && executable.trim() !== '';

  return (
    <form
      className="clipped-field"
      aria-label="Register a game"
      onSubmit={(event: FormEvent): void => {
        event.preventDefault();
        if (!ready) {
          return;
        }
        void editor
          .apply(REGISTERING, () =>
            registerGame(
              name.trim(),
              executable.trim(),
              fragment.trim() === '' ? null : fragment.trim(),
            ),
          )
          .then(() => {
            setName('');
            setExecutable('');
            setFragment('');
          });
      }}
    >
      <label className="clipped-field__label">
        Game
        <input
          className="clipped-input"
          type="text"
          value={name}
          disabled={busy}
          onChange={(event): void => {
            setName(event.target.value);
          }}
        />
      </label>
      <label className="clipped-field__label">
        Executable
        <input
          className="clipped-input"
          type="text"
          value={executable}
          disabled={busy}
          placeholder="mygame.exe"
          onChange={(event): void => {
            setExecutable(event.target.value);
          }}
        />
      </label>
      <label className="clipped-field__label">
        Folder must contain
        <input
          className="clipped-input"
          type="text"
          value={fragment}
          disabled={busy}
          onChange={(event): void => {
            setFragment(event.target.value);
          }}
        />
      </label>
      {/*
       * Said here rather than only in a help page, because it is the field
       * nobody expects: two games can ship the same executable name — `hl2.exe`
       * is both Half-Life 2 and Team Fortress 2 — and the fragment is what
       * tells them apart.
       */}
      <p className="clipped-panel__body clipped-muted">
        The executable is a file name, never a path. Fill in the folder only when the name alone
        would match another game too.
      </p>
      <button type="submit" className="clipped-btn clipped-btn--primary" disabled={busy || !ready}>
        Add game
      </button>
    </form>
  );
}

/**
 * What a registration in flight is named.
 *
 * `CatalogueEditor.pending` holds a `game_id`, and a registration has none yet
 * — the recorder derives it. This is not a valid identifier (`GameKey::parse`
 * takes `[a-z0-9-]`), so it can never collide with a real entry's row.
 */
const REGISTERING = 'registering a new game';
