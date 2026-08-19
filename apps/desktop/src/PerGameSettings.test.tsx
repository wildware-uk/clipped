import type { LibraryGame, SettingsView } from '@clipped/shared';
import { cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent, { type UserEvent } from '@testing-library/user-event';
import { MemoryRouter } from 'react-router';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { SettingsScreen } from './SettingsScreen';
import { stubRecorderLinkRuntime, type StubbedRuntime } from './test/recorderLinkRuntime';
import schema from '../../../packages/shared/src/ipc/protocol-schema.json';

/**
 * The per-game settings page's contract, as tests (SPEC.md section 31, issue
 * #63).
 *
 * Three properties, one per acceptance criterion, and each is one that would
 * pass for the wrong reason if it were asserted loosely.
 *
 * The first is that **inherited and overridden are told apart, in words**. The
 * trap is specific: "the override is shown" passes just as well on a build that
 * marks *every* row as this game's, which is the build where Reset appears
 * against values nobody set and clearing one changes nothing. So every case
 * below asserts the pair — this row says one thing and that row says the other
 * — rather than looking for one word somewhere on the page.
 *
 * The second is that **a change reaches the next recording without a restart**,
 * and that the page says exactly that and no more. It cannot reach a running
 * encoder and does not claim to; it does not need a restart and does not say it
 * does (AGENTS.md section 27).
 *
 * The third is that **nothing on the page silently does nothing**. Reset is
 * drawn only where there is something to clear, a setting no recording reads is
 * drawn as a sentence rather than as a control, and a recorder too old to
 * understand the scope is said to be too old rather than having its answer drawn
 * under a game's name.
 *
 * # Where the fixtures come from
 *
 * `protocol-schema.json`, which `cargo run -p clipped-ipc --bin protocol-schema`
 * writes from the Rust types themselves. The per-game view these tests drive the
 * screen with is the recorder's own exemplar, field for field — a shape typed
 * out here by hand would go on passing after the protocol moved underneath it.
 */

/** A sample frame the Rust build produced, by the name it recorded it under. */
function sample(name: string): Record<string, unknown> {
  const samples = schema.samples as { name: string; frame: Record<string, unknown> }[];
  const found = samples.find((candidate) => candidate.name === name);
  if (found === undefined) {
    throw new Error(
      `protocol-schema.json has no sample called "${name}"; regenerate it, or the name has moved`,
    );
  }
  return found.frame;
}

/**
 * One game's settings, as the recorder itself describes them.
 *
 * The exemplar carries all three states on purpose: `microphone` is set for the
 * game, `framerate` is inherited, and `capture_target` is a key the file holds
 * and no recording reads.
 */
const FOR_GAME = (
  sample("one game's settings: one it set, one it inherits, and one nothing reads") as {
    outcome: { ok: { settings: SettingsView } };
  }
).outcome.ok.settings;

/** The game that view is about. */
const GAME = FOR_GAME.game ?? '';

/** The setting the exemplar says that game set. */
const SET_HERE = 'Microphone';

/** The setting the exemplar says it inherits. */
const INHERITED = 'Frame rate';

/** The global page, which is what the section reads its list of games from. */
const GLOBAL: SettingsView = {
  file: FOR_GAME.file,
  games: FOR_GAME.games ?? [],
  settings: [],
};

/** The games the library has recordings of. */
const PLAYED: LibraryGame[] = [
  {
    game_id: GAME,
    name: 'Counter-Strike 2',
    sessions: 3,
    recordings: 9,
    clips: 1,
    favourites: 0,
    bytes: 1,
    missing: 0,
  },
];

/** The same per-game view with some settings answered differently. */
function viewWith(changes: Partial<SettingsView>): SettingsView {
  return { ...FOR_GAME, ...changes };
}

/** Every setting on the per-game view reported as this game's own. */
function allOverridden(): SettingsView {
  return viewWith({
    settings: FOR_GAME.settings.map((entry) => ({ ...entry, overridden: true })),
  });
}

/** Every setting on it reported as inherited, which is a game with no section. */
function allInherited(): SettingsView {
  return viewWith({
    games: [],
    settings: FOR_GAME.settings.map((entry) => ({ ...entry, overridden: false })),
  });
}

/** What the window sent to `apply_recorder_settings`, in order. */
function saved(runtime: StubbedRuntime): readonly Record<string, unknown>[] {
  return runtime.invocations
    .filter((invocation) => invocation.command === 'apply_recorder_settings')
    .map((invocation) => invocation.args);
}

/** What the window asked `recorder_settings`, in order. */
function reads(runtime: StubbedRuntime): readonly Record<string, unknown>[] {
  return runtime.invocations
    .filter((invocation) => invocation.command === 'recorder_settings')
    .map((invocation) => invocation.args);
}

function renderScreen(
  overrides: Parameters<typeof stubRecorderLinkRuntime>[2] = {},
): StubbedRuntime {
  const runtime = stubRecorderLinkRuntime({ link: 'connecting' }, null, {
    recorderSettings: (args) => (args['game'] === undefined ? GLOBAL : FOR_GAME),
    games: () => Promise.resolve(PLAYED),
    audioDevices: () => ({ microphones: [] }),
    recorderHotkeys: () => [],
    ...overrides,
  });
  render(
    <MemoryRouter>
      <SettingsScreen />
    </MemoryRouter>,
  );
  return runtime;
}

/** Opens the section and picks the game, leaving its page on screen. */
async function openGame(user: UserEvent, game = GAME): Promise<HTMLElement> {
  await user.click(screen.getByRole('tab', { name: 'Per game' }));
  await waitFor(() => {
    expect(
      within(screen.getByRole('combobox', { name: 'Game' })).getAllByRole('option').length,
      'the chooser never filled in',
    ).toBeGreaterThan(1);
  });
  await user.selectOptions(screen.getByRole('combobox', { name: 'Game' }), game);
  return waitFor(() => screen.getByRole('form', { name: `Settings for ${game}` }));
}

/** The block a named control sits in, which is where its state and Reset are. */
function fieldFor(label: string): HTMLElement {
  const control = screen.getByRole(label === SET_HERE ? 'combobox' : 'textbox', {
    name: new RegExp(label),
  });
  const field = control.closest('.clipped-field');
  if (field === null) {
    throw new Error(`the "${label}" control is not in a field`);
  }
  return field as HTMLElement;
}

describe('the per-game settings page', () => {
  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
  });

  /*
   * Issue #63's first criterion. Both halves in one case, because either half
   * alone passes on a build that draws every row the same way — and that build
   * is the one where Reset is offered for values nobody set, which is the
   * control that does nothing AGENTS.md section 27 forbids.
   */
  it('says in words which values this game set and which it inherits, and marks them differently', async () => {
    const user = userEvent.setup();
    renderScreen();
    await openGame(user);

    const mine = fieldFor(SET_HERE);
    const theirs = fieldFor(INHERITED);

    expect(within(mine).getByText('Set for this game')).toBeInTheDocument();
    expect(within(theirs).getByText('Inherited')).toBeInTheDocument();
    expect(
      within(mine).queryByText('Inherited'),
      'a value this game set must not also read as inherited',
    ).not.toBeInTheDocument();
    expect(
      within(theirs).queryByText('Set for this game'),
      'an inherited value must not read as one this game set',
    ).not.toBeInTheDocument();

    // The second signal, and not a colour either: Reset exists exactly where
    // there is something to clear.
    expect(within(mine).getByRole('button', { name: `Reset ${SET_HERE}` })).toBeInTheDocument();
    expect(
      within(theirs).queryByRole('button', { name: `Reset ${INHERITED}` }),
      'Reset on a value this game never set would clear nothing',
    ).not.toBeInTheDocument();
  });

  /*
   * The state the page opens in for a game nobody has configured, and the one a
   * screen most easily gets wrong: every value is real and none of them is this
   * game's.
   */
  it('offers no reset at all for a game the settings file says nothing about', async () => {
    const user = userEvent.setup();
    renderScreen({
      recorderSettings: (args) =>
        args['game'] === undefined ? { ...GLOBAL, games: [] } : allInherited(),
    });
    const form = await openGame(user);

    expect(within(form).getAllByText('Inherited').length).toBeGreaterThan(0);
    expect(within(form).queryByText('Set for this game')).not.toBeInTheDocument();
    expect(
      within(form).queryAllByRole('button', { name: /^Reset / }),
      'nothing was set for this game, so there is nothing to reset',
    ).toHaveLength(0);
  });

  /*
   * The opposite mistake, stated as its own case: a build that marked every row
   * as this game's would pass the case above and fail here, which is what makes
   * the pair a measurement rather than a search for a word.
   */
  it('offers a reset on every value when the recorder says the game set every value', async () => {
    const user = userEvent.setup();
    renderScreen({
      recorderSettings: (args) => (args['game'] === undefined ? GLOBAL : allOverridden()),
    });
    const form = await openGame(user);

    expect(within(form).queryByText('Inherited')).not.toBeInTheDocument();
    expect(
      within(form).getAllByRole('button', { name: /^Reset / }).length,
      'every value is this game’s, so every control it has can be cleared',
    ).toBe(within(form).getAllByText('Set for this game').length);
  });

  /* Every read and every write names the game, or it is about something else. */
  it('asks and saves against the game rather than against the global settings', async () => {
    const user = userEvent.setup();
    const runtime = renderScreen({
      applySettings: () => FOR_GAME,
    });
    await openGame(user);

    expect(reads(runtime).map((args) => args['game'])).toContain(GAME);

    await user.click(screen.getByRole('button', { name: `Reset ${SET_HERE}` }));
    await waitFor(() => {
      expect(saved(runtime)).toHaveLength(1);
    });
    expect(
      saved(runtime)[0]?.['game'],
      'a reset that named no game would clear it for every game',
    ).toBe(GAME);
    expect(saved(runtime)[0]?.['values']).toEqual({ microphone: null });
  });

  /*
   * Save, which is the other control on the page. Only what was edited goes,
   * and it goes against the game: a Save that sent every value would turn every
   * inherited setting on the page into an override for that game, which is a
   * control doing far more than it says (AGENTS.md section 27).
   */
  it('sends only what was edited, against the game, when Save is pressed', async () => {
    const user = userEvent.setup();
    const runtime = renderScreen({ applySettings: () => FOR_GAME });
    await openGame(user);

    const framerate = within(fieldFor(INHERITED)).getByRole('textbox');
    await user.clear(framerate);
    await user.type(framerate, '120');
    await user.click(screen.getByRole('button', { name: `Save changes for ${GAME}` }));

    await waitFor(() => {
      expect(saved(runtime)).toHaveLength(1);
    });
    expect(saved(runtime)[0]?.['game']).toBe(GAME);
    expect(
      saved(runtime)[0]?.['values'],
      'only the edited setting: the rest are inherited and must stay that way',
    ).toEqual({ framerate: '120' });
  });

  /*
   * A setting the file can carry for this game and no recording reads. The same
   * rule the global page follows: the value and the recorder's own sentence,
   * never a control (AGENTS.md section 27).
   */
  it('draws a setting nothing reads as its value and the reason, not as a control', async () => {
    const user = userEvent.setup();
    renderScreen();
    const form = await openGame(user);

    const unread = FOR_GAME.settings.find((entry) => !entry.applies);
    expect(unread, 'the exemplar should carry a setting nothing reads').toBeDefined();
    expect(within(form).getByText(unread?.unavailable ?? '')).toBeInTheDocument();
    expect(
      within(form).queryByRole('combobox', { name: new RegExp(unread?.label ?? '') }),
      'a setting no recording reads must not be drawn as a working control',
    ).not.toBeInTheDocument();
  });

  /*
   * Issue #63's second criterion, from the side this window can actually
   * establish: the page says when a change counts, and it says the thing that
   * is true. `get_settings` gained the scope in this issue, so a recorder that
   * ignores it answers the global settings — and drawing those under a game's
   * name would show every value as inherited when the global settings had set
   * half of them.
   */
  it('says the recorder is too old rather than drawing the global settings under a game’s name', async () => {
    const user = userEvent.setup();
    renderScreen({
      // What a recorder built before this issue answers: the global settings,
      // with no game on them at all.
      recorderSettings: () => ({ ...GLOBAL, settings: FOR_GAME.settings }),
    });
    await user.click(screen.getByRole('tab', { name: 'Per game' }));
    await waitFor(() => {
      expect(
        within(screen.getByRole('combobox', { name: 'Game' })).getAllByRole('option').length,
      ).toBeGreaterThan(1);
    });
    await user.selectOptions(screen.getByRole('combobox', { name: 'Game' }), GAME);

    await waitFor(() => {
      expect(screen.getByText(/older than this window/)).toBeInTheDocument();
    });
    expect(
      screen.queryByRole('form', { name: `Settings for ${GAME}` }),
      'no form at all: every control on it would be about the wrong scope',
    ).not.toBeInTheDocument();
  });

  /*
   * The list is two sources joined, and neither is the catalogue: a game that
   * has never been recorded and has never been configured cannot be opened here
   * (issue #245). What matters is that the page says which list it is drawing
   * rather than presenting it as every game on this machine.
   */
  it('lists the games with recordings and the games with settings, and says what is missing', async () => {
    const user = userEvent.setup();
    renderScreen({
      recorderSettings: (args) =>
        args['game'] === undefined ? { ...GLOBAL, games: ['minecraft'] } : FOR_GAME,
    });
    await user.click(screen.getByRole('tab', { name: 'Per game' }));

    const chooser = await waitFor(() => screen.getByRole('combobox', { name: 'Game' }));
    await waitFor(() => {
      expect(within(chooser).getAllByRole('option').length).toBe(3);
    });
    const options = within(chooser)
      .getAllByRole('option')
      .map((option) => (option as HTMLOptionElement).value);
    expect(options).toContain(GAME);
    expect(options, 'a game with settings and no recordings is still configurable').toContain(
      'minecraft',
    );
    expect(screen.getByText(/no command reads it \(issue #245\)/)).toBeInTheDocument();
  });
});
