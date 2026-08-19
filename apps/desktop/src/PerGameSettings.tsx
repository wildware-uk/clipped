import type { AudioDevices } from '@clipped/shared';
import { useState, type ReactNode } from 'react';

import { useGames, type LibraryRead } from './library';
import { Field, NotInForce, type SettingScope } from './SettingField';
import { answeredForGame, describeSettingsProblem, gameChoices, useSettings } from './settings';

/**
 * One game's settings, on the Settings screen (issue #63).
 *
 * # What this page is, and what it is not
 *
 * It is the same eight settings the Recording and Audio sections draw, resolved
 * against one game instead of against nothing. Every value on it is real and
 * applies to that game; what changes is where it came from, and that is the
 * whole of what the page is for (AGENTS.md section 30).
 *
 * It is **not** the global page with a game attached. The layering is the
 * recorder's — `get_settings` takes the game and answers with what each setting
 * resolves to *and* whether that game set it — and this window never reads two
 * pages and folds them together. The fold destroys the one distinction that
 * matters: a game that pins 60 while the global settings say 60 has set the
 * value, and its Reset does something, and it will not follow the global
 * settings when they move (`crates/session/src/config/value.rs`).
 *
 * # Why the game list is short, and says so
 *
 * The games it offers are the games the settings file already has a section for
 * and the games the library has recordings of. What it cannot offer is a game
 * that is neither: which processes Clipped would recognise is the catalogue's
 * answer, no protocol command reads it, and this window has no permission to
 * read the file (issue #245). So the page says which list it is drawing rather
 * than presenting it as every game on the machine (AGENTS.md section 27).
 */

/** The scope every control on this page is in. */
const SCOPE: SettingScope = { kind: 'game' };

/** The element id of the game chooser, so its label can name it. */
const CHOOSER = 'per-game-chooser';

/** One game's settings, and the controls that change them. */
function GameSettings({
  game,
  devices,
}: {
  readonly game: string;
  readonly devices: LibraryRead<AudioDevices>;
}): ReactNode {
  const settings = useSettings(game);
  const [edits, setEdits] = useState<Record<string, string>>({});

  if (settings.read.state === 'reading') {
    return <p className="clipped-panel__body">Asking the recorder…</p>;
  }

  if (settings.read.state === 'unread') {
    return (
      <p className="clipped-panel__body" role="status">
        {describeSettingsProblem(settings.read.problem)}
      </p>
    );
  }

  const view = settings.read.value;

  /*
   * A recorder that ignored the game answered about something else. Before
   * issue #63 `get_settings` took no parameters at all, so an older recorder
   * replies with the global settings — where every value would read as
   * inherited when the global settings had set half of them, and where Reset
   * would clear a value for every game (AGENTS.md sections 27 and 45).
   */
  if (!answeredForGame(view, game)) {
    return (
      <p className="clipped-panel__body" role="status">
        The recorder that is running is older than this window and has no per-game settings: it
        answered with the global settings instead. Restarting Clipped starts the recorder that came
        with it.
      </p>
    );
  }

  const entries = view.settings;
  const changed = entries.some((entry) => edits[entry.key] !== undefined);

  const save = (): void => {
    const values: Record<string, string | null> = {};
    for (const entry of entries) {
      const edited = edits[entry.key];
      if (edited !== undefined) {
        values[entry.key] = edited;
      }
    }
    void settings.apply(values).then((applied) => {
      // Cleared only for what the recorder took. A refused value stays on
      // screen to be corrected rather than retyped (AGENTS.md section 45).
      if (applied) {
        setEdits({});
      }
    });
  };

  return (
    <form
      aria-label={`Settings for ${game}`}
      onSubmit={(event) => {
        event.preventDefault();
        save();
      }}
    >
      {entries.map((entry) =>
        entry.applies ? (
          <Field
            key={entry.key}
            entry={entry}
            devices={devices}
            scope={SCOPE}
            edited={edits[entry.key]}
            onEdit={(value) => {
              setEdits((current) => ({ ...current, [entry.key]: value }));
            }}
            onReset={() => {
              // An edit to the setting being reset is dropped: Reset is what
              // was asked for, and a half-typed value left behind it would be
              // sent by the next Save.
              setEdits((current) =>
                Object.fromEntries(Object.entries(current).filter(([key]) => key !== entry.key)),
              );
              void settings.apply({ [entry.key]: null });
            }}
          />
        ) : (
          /*
           * A setting the file can carry for this game and no recording reads.
           * Drawn as its value and the recorder's sentence, never as a working
           * control — the same rule the global page follows, and the reason it
           * is the same component (AGENTS.md section 27).
           */
          <NotInForce key={entry.key} entry={entry} />
        ),
      )}

      {settings.save.state === 'refused' ? (
        <p className="clipped-panel__body" role="alert">
          {describeSettingsProblem(settings.save.problem)}
        </p>
      ) : null}

      <div className="clipped-field">
        <button
          type="submit"
          className="clipped-btn clipped-btn--primary"
          disabled={!changed || settings.save.state === 'saving'}
        >
          {settings.save.state === 'saving' ? 'Saving…' : `Save changes for ${game}`}
        </button>
        {changed ? (
          <button
            type="button"
            className="clipped-btn clipped-btn--secondary"
            onClick={() => {
              setEdits({});
            }}
          >
            Discard changes
          </button>
        ) : null}
      </div>
    </form>
  );
}

/** What the per-game section is given. */
export interface PerGameSettingsProps {
  /**
   * This machine's microphones, so that the microphone control offers the same
   * list here as on the global page.
   *
   * Passed in rather than asked for again: the screen already holds one
   * subscription that re-asks when the window comes back to the front, and a
   * second would enumerate the endpoints twice for one answer (`settings.ts`).
   */
  readonly devices: LibraryRead<AudioDevices>;
}

/** The per-game settings page: pick a game, then change what it records at. */
export function PerGameSettings({ devices }: PerGameSettingsProps): ReactNode {
  // The global page, read for one thing only: which games the settings file
  // already holds a section for. It is the recorder's answer rather than a
  // guess from the library, because a game can be configured and never
  // recorded.
  const global = useSettings();
  const library = useGames();
  const [chosen, setChosen] = useState('');

  const configured = global.read.state === 'read' ? (global.read.value.games ?? []) : [];
  const games = gameChoices(configured, library.state === 'read' ? library.value : []);
  const open = games.some((game) => game.id === chosen) ? chosen : '';

  return (
    <section className="clipped-panel" aria-label="Per-game settings">
      <h3 className="clipped-panel__heading">Which game</h3>
      <p className="clipped-panel__body clipped-muted">
        The games Clipped has recordings of, and the games the settings file already has a section
        for. A game that is neither cannot be opened here yet: the list of games Clipped would
        recognise is the recorder’s catalogue, and no command reads it (issue #245).
      </p>

      {global.read.state === 'unread' ? (
        <p className="clipped-panel__body" role="status">
          {describeSettingsProblem(global.read.problem)}
        </p>
      ) : null}

      <div className="clipped-field">
        <label className="clipped-field__label" htmlFor={CHOOSER}>
          Game
        </label>
        <select
          className="clipped-input"
          id={CHOOSER}
          value={open}
          onChange={(event) => {
            setChosen(event.target.value);
          }}
        >
          <option value="">Choose a game…</option>
          {games.map((game) => (
            <option key={game.id} value={game.id}>
              {/*
               * Which games already have a section is worth saying in the list
               * itself: it is the difference between opening a page to change
               * something and opening one to see what a game inherits.
               */}
              {game.configured ? `${game.label} (has its own settings)` : game.label}
            </option>
          ))}
        </select>
      </div>

      {games.length === 0 && global.read.state === 'read' && library.state === 'read' ? (
        <p className="clipped-panel__body" role="status">
          No game has been recorded and none has settings of its own, so there is nothing to
          configure per game yet. Clipped records a game because it launched; play one and it
          appears here.
        </p>
      ) : null}

      {/*
       * Keyed by the game, so that choosing another one mounts a page rather
       * than re-using this one. Re-using it would leave the previous game's
       * answers — and the edits typed against them — on screen under the new
       * game's name until its read came back (AGENTS.md section 27).
       */}
      {open === '' ? null : <GameSettings key={open} game={open} devices={devices} />}
    </section>
  );
}
