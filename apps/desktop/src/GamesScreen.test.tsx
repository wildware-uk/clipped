import { cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { StrictMode } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { App } from './App';
import { describeGameDetection } from './gameDetection';
import { GamesScreen } from './GamesScreen';
import { stubRecorderLinkRuntime } from './test/recorderLinkRuntime';
import type { RecorderLinkState } from './useRecorderLink';

/**
 * The Games screen's contract, as tests (issue #107).
 *
 * The properties worth guarding here are the ones that would rot silently.
 *
 * The first is that the screen shows the recorder's real state rather than a
 * sentence somebody typed. A screen whose wording is a constant looks identical
 * to one that is following the link, and stays identical after the link has
 * been disconnected from it — which is why the case below drives the whole
 * application and moves the link underneath it rather than rendering
 * `describeGameDetection`'s output next to itself.
 *
 * The second is that no wording claims more than the link can establish. The
 * link sees the recorder this window has and nothing else, so a state that said
 * games were going undetected would be a claim about the machine that nothing
 * here measured.
 *
 * The third is that the screen offers nothing that does nothing, and does offer
 * the one thing that works. The deck draws an Add Game button and a table of
 * games; neither can be honoured in this build (AGENTS.md section 27), so
 * neither is drawn, and the assertions are about the absence rather than about
 * the presence of an explanation for it. What is drawn instead — the command
 * that records a game today, and the four things this screen owes with the
 * issue against each — is asserted by substance rather than by shape, because a
 * review showed both could be emptied without a case noticing.
 */

/** Mounts the application the way `main.tsx` does, StrictMode and all. */
function renderApp(): void {
  render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}

/** Opens the Games screen from the sidebar. */
async function openGames(user: ReturnType<typeof userEvent.setup>): Promise<void> {
  await user.click(screen.getByRole('link', { name: 'Games' }));
}

/** The live region the detection state is drawn in. */
function detectionPanel(): HTMLElement {
  return screen.getByRole('region', { name: 'Game detection' });
}

/*
 * Every state the link has, and what each one means for game detection. The
 * five are listed rather than generated: the failure this guards against is
 * two states collapsing into one wording, and a table built from the states
 * themselves could not see that happen.
 *
 * At module scope because the rendering cases below need the same five: what
 * the screen offers somebody to do is the same in all of them, and a case that
 * only checked one would not notice the sentence moving into a branch.
 */
const STATES: readonly (readonly [string, RecorderLinkState | null, string, RegExp])[] = [
  ['outside the Clipped window', null, 'Not known', /not the Clipped window.*no recorder to ask/],
  ['while the link is being made', { link: 'connecting' }, 'Not known yet', /Looking for/],
  [
    'while the link is being remade',
    {
      link: 'reconnecting',
      attempt: 2,
      attempts_allowed: 4,
      delay_ms: 500,
      reason: 'The pipe closed.',
    },
    'Not known',
    /The pipe closed\. Attempt 2 of 4\./,
  ],
  [
    'when there is no recorder',
    { link: 'unavailable', reason: 'clipped-recorder.exe is not beside this application.' },
    'Not known',
    /not attached to a recorder.*not beside this application/,
  ],
  [
    'when a recorder that does not record games by itself is attached',
    { link: 'attached', recorder_process_id: 7, features: [], status: { state: 'idle' } },
    'This recorder is not detecting games',
    /did not say it records games by itself.*clipped-recorder watch/,
  ],
];

/**
 * A recorder that advertised `automatic`: one started with
 * `--watch-for-games`, which is what the supervisor starts.
 *
 * Deliberately outside {@link STATES}. It is a sixth rendering rather than a
 * variant of the fifth, and the cases below that walk `STATES` are about the
 * five that share a sentence — this one does not share it, which is the point.
 */
const WATCHING: RecorderLinkState = {
  link: 'attached',
  recorder_process_id: 7,
  features: ['automatic'],
  status: { state: 'watching' },
};

describe('what the Games screen says about detection', () => {
  it.each(STATES)('is described %s', (_case, link, state, detail) => {
    const described = describeGameDetection(link);

    expect(described.state).toBe(state);
    expect(described.detail).toMatch(detail);
  });

  /*
   * The link sees one thing: the recorder this window started or attached to. A
   * `clipped-recorder watch` started in a terminal serves no protocol and is
   * invisible to it — and that is exactly what the sentence directly beneath
   * the state recommends. So a rendering that said games were going undetected
   * would be stating something the window has not looked at, and would
   * contradict its own next paragraph (AGENTS.md section 27).
   *
   * So a state is only ever one of two things: "Not known...", which claims
   * nothing, or "This recorder...", which names what it is a claim about. The
   * assertion is on the state alone and deliberately not on the state and its
   * detail together. The state is the panel's `<h2>`; a heading is read on its
   * own, skipped to on its own by a screen reader, and is the part somebody
   * takes away. An earlier draft of this case allowed the detail to supply the
   * scope, and a break that put "Not detecting games" back at the top of the
   * attached rendering — leaving the explanation beneath it intact — went
   * straight through it.
   *
   * Asserted over all five renderings rather than over the one that was wrong,
   * because the defect is a class: it is the same unscoped heading whichever
   * branch it gets written into.
   */
  it.each(STATES)(
    'either names the recorder it speaks for, or claims nothing, %s',
    (_case, link) => {
      expect(describeGameDetection(link).state).toMatch(/^(Not known|This recorder )/);
    },
  );

  /*
   * Issue #587, from the side that reads it. The two attached renderings differ
   * by exactly one thing — whether the recorder advertised `automatic` — and
   * until that issue no recorder advertised it, so this screen told everybody
   * that the recorder it was attached to was not detecting games. Since issue
   * #421 that is false of the recorder the desktop application itself starts:
   * `SupervisorSettings::watch_for_games` passes `--watch-for-games`.
   *
   * Both directions, because a screen that said "watching" for every attached
   * recorder would pass half of it and would be the same defect pointing the
   * other way.
   */
  it('tells a recorder that records games by itself from one that never will', () => {
    expect(describeGameDetection(WATCHING).state).toBe('This recorder is watching for games');
    expect(describeGameDetection(WATCHING).detail).toMatch(/records games by itself/);

    const asked: RecorderLinkState = { ...WATCHING, features: [] };
    expect(describeGameDetection(asked).state).toBe('This recorder is not detecting games');
  });

  /*
   * The rule issue #447 settled, applied to this capability: "cannot" and "not
   * known yet" are different answers, and a link that has not attached has
   * established neither. A screen that read the absent feature as "no" would
   * say a recorder is not detecting games before it has found one.
   */
  it('never reads a link with no recorder as a recorder that cannot detect games', () => {
    for (const link of [
      null,
      { link: 'connecting' } as const,
      { link: 'unavailable', reason: 'no recorder' } as const,
    ]) {
      expect(describeGameDetection(link).state).toMatch(/^Not known/);
    }
  });

  /*
   * The reason a recorder could not be reached is the only part of that state a
   * user can act on, and it is the recorder's own words. A rendering that
   * dropped it would leave "Not known" and nothing to do about it (AGENTS.md
   * section 45).
   */
  it('carries the recorder link its own reason rather than a generic sentence', () => {
    const described = describeGameDetection({
      link: 'unavailable',
      reason: 'The endpoint could not be named: access denied.',
    });

    expect(described.detail).toContain('The endpoint could not be named: access denied.');
  });

  /*
   * A link that has not settled has established nothing, so the two states it
   * can be in say so and make no claim about detection in either direction.
   *
   * Asserted positively. This case used to read `not.toBe('Not detecting
   * games')`, which went on passing the moment that exact string stopped being
   * used anywhere — a check on a spelling rather than on the property.
   */
  it('says nothing about detection while the link has not settled', () => {
    for (const link of [null, { link: 'connecting' } as const]) {
      const described = describeGameDetection(link);

      expect(described.state).toMatch(/^Not known/);
      expect(`${described.state} ${described.detail}`).not.toMatch(/detect/i);
    }
  });
});

describe('the Games screen', () => {
  beforeEach(() => {
    window.location.hash = '';
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
    window.location.hash = '';
  });

  it('follows the recorder link rather than showing a sentence that was typed once', async () => {
    const user = userEvent.setup();
    const runtime = stubRecorderLinkRuntime({ link: 'connecting' });
    renderApp();
    await openGames(user);

    // Anchored, because "Not known" is a substring of "Not known yet" and an
    // unanchored match would let the screen sit on one wording for both.
    await waitFor(() => {
      expect(within(detectionPanel()).getByRole('heading', { level: 2 })).toHaveTextContent(
        /^Not known yet$/,
      );
    });

    runtime.emit({
      event: 'state',
      link: 'unavailable',
      reason: 'clipped-recorder.exe is not beside this application.',
    });

    await waitFor(() => {
      expect(within(detectionPanel()).getByRole('heading', { level: 2 })).toHaveTextContent(
        /^Not known$/,
      );
    });
    expect(
      within(detectionPanel()).getByText(/clipped-recorder\.exe is not beside this application\./),
    ).toBeVisible();

    // And on to the one state that can say anything about detection, so that
    // the case covers a move in both directions rather than one wording giving
    // way to another that happens to differ.
    runtime.emit({
      event: 'state',
      link: 'attached',
      recorder_process_id: 7,
      features: [],
      status: { state: 'idle' },
    });

    await waitFor(() => {
      expect(within(detectionPanel()).getByRole('heading', { level: 2 })).toHaveTextContent(
        /^This recorder is not detecting games$/,
      );
    });
  });

  /*
   * A screen that only says what is missing leaves somebody with nothing to do
   * (AGENTS.md section 45). Automatic recording is built and running today, so
   * the panel names the command that does it and the issue that would bring it
   * into this window.
   *
   * Checked in every link state, because it is true in every link state and the
   * failure worth catching is the sentence sliding into one branch of
   * `describeGameDetection` — where it would be missing precisely when the
   * window cannot reach a recorder, which is when the reader most needs it.
   *
   * The match is on the middle of the sentence rather than on
   * `WHAT_WORKS_TODAY` itself. Importing the constant and asserting the screen
   * renders it is a tautology that survives the constant being emptied; this
   * asserts the two things the sentence is for — a command that can be run now,
   * and where to follow the work that removes the need to. "records a game as
   * it launches" is also absent from every state detail, so this cannot be
   * satisfied by the attached rendering's mention of the same command.
   */
  it.each(STATES)('offers the one thing somebody can do today, %s', (_case, link) => {
    render(<GamesScreen link={link} />);

    const offer = within(detectionPanel()).getByText(
      /clipped-recorder watch records a game as it launches/,
    );
    expect(offer).toBeVisible();
    expect(offer).toHaveTextContent(/from a terminal/);
    expect(offer).toHaveTextContent(/issue #241/i);
  });

  /*
   * And the sixth rendering, where that sentence would be wrong: telling
   * somebody to start `clipped-recorder watch` beside a recorder that is
   * already watching would have two watchers racing for the same game. The
   * screen still has to offer something rather than only describe (AGENTS.md
   * section 45), and what it offers is where the recordings went.
   */
  it('does not tell somebody to start a watcher beside a recorder that is watching', () => {
    render(<GamesScreen link={WATCHING} />);

    const panel = detectionPanel();
    expect(within(panel).queryByText(/clipped-recorder watch records a game/)).toBeNull();
    expect(within(panel).getByText(/records a game as it launches/)).toBeVisible();
    expect(within(panel).getByText(/in the Library/)).toBeVisible();
  });

  it('announces a change of state rather than only drawing it', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime({ link: 'connecting' });
    renderApp();
    await openGames(user);

    expect(detectionPanel()).toHaveAttribute('aria-live', 'polite');
  });

  /*
   * The deck draws an Add Game button, a row menu and an Edit Recording
   * Settings button against every game. None of them has anything behind it in
   * this build - the catalogue cannot be read or written from this window
   * (issue #245) - so none is drawn. This asserts the property rather than the
   * three cases: anything operable inside the screen would have to do
   * something, and nothing here can.
   */
  it('offers no control, because nothing it would drive can be reached', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime({
      link: 'attached',
      recorder_process_id: 7,
      features: [],
      status: { state: 'idle' },
    });
    renderApp();
    await openGames(user);

    const main = screen.getByRole('main');
    expect(within(main).queryAllByRole('button')).toHaveLength(0);
    expect(within(main).queryAllByRole('link')).toHaveLength(0);
    expect(within(main).queryAllByRole('textbox')).toHaveLength(0);
    expect(within(main).queryAllByRole('combobox')).toHaveLength(0);
    expect(within(main).queryAllByRole('checkbox')).toHaveLength(0);
    expect(within(main).queryAllByRole('radio')).toHaveLength(0);
  });

  /*
   * An empty Game / Recording / Last played table is indistinguishable from a
   * machine that has played nothing, so a screen that cannot look must say so
   * rather than draw the headings over nothing.
   *
   * This was once asserted by requiring a "what is missing" table in its place.
   * That table is gone — the screen owes nothing now — so the claim is made
   * directly: no table of games, and a sentence saying which of the two silences
   * this is.
   */
  it('draws no table of games when it has not been able to look', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime({
      link: 'attached',
      recorder_process_id: 7,
      features: [],
      status: { state: 'idle' },
    });
    renderApp();
    await openGames(user);

    expect(screen.queryByRole('table', { name: 'Games Clipped knows' })).toBeNull();
    expect(screen.queryByRole('table', { name: 'Games recorded' })).toBeNull();
    expect(await screen.findByText(/cannot list its catalogue/i)).toBeVisible();
  });

  /*
   * The replacement for "names each thing it owes, and the issue that lands
   * it", and the regression that case would not have caught.
   *
   * That case asserted the screen listed its remaining work with an issue
   * against each. Its last row was the catalogue controls — registering,
   * renaming, excluding — which #245 landed. The controls were drawn and the
   * row stayed, so the screen offered four working buttons and told the reader
   * underneath that none of them existed.
   *
   * Stale in exactly one direction, which is the direction that needs a guard:
   * a screen is far more likely to keep claiming it cannot do something than to
   * claim it can. So the assertion is on the absence of the claim, wherever it
   * is worded, rather than on the table that used to carry it.
   */
  it('does not say the catalogue controls are missing while it is drawing them', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime(
      {
        link: 'attached',
        recorder_process_id: 7,
        features: ['catalogue', 'catalogue_editing'],
        status: { state: 'idle' },
      },
      null,
      {
        catalogue: () =>
          Promise.resolve([
            {
              game_id: 'counter-strike-2',
              name: 'Counter-Strike 2',
              source: 'shipped',
              executables: [{ name: 'cs2.exe' }],
              excluded: false,
            },
          ]),
      },
    );
    renderApp();
    await openGames(user);

    // The controls are there.
    expect(await screen.findByRole('form', { name: 'Register a game' })).toBeVisible();

    // And nothing on the screen describes them as work still to come.
    expect(screen.queryByText(/adding an unknown executable/i)).toBeNull();
    expect(screen.queryByText(/what this screen will show/i)).toBeNull();
    expect(screen.queryByRole('table', { name: 'What the Games screen will show' })).toBeNull();
  });

  /*
   * Reached with the keyboard alone, and focus lands in the screen afterwards
   * so that a screen reader announces the change. The shell owns both
   * behaviours; this checks them through the Games link specifically, because
   * "keyboard navigable" is one of issue #107's three acceptance criteria and a
   * screen that is only reachable by mouse fails it whatever the shell does.
   */
  it('is reached with Tab and Enter, and takes focus when it opens', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime({ link: 'connecting' });
    renderApp();

    // Skip link, Home, Library, Games.
    await user.tab();
    await user.tab();
    await user.tab();
    await user.tab();
    expect(document.activeElement).toHaveTextContent('Games');

    await user.keyboard('{Enter}');

    expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent('Games');
    expect(screen.getByRole('main')).toHaveFocus();
  });

  /*
   * Rendered on its own rather than through the shell, so that the screen's own
   * heading structure is the subject: a screen whose only heading was its title
   * gives a screen-reader user nothing to navigate between.
   */
  it('has a heading for each of its parts', () => {
    render(<GamesScreen link={null} />);

    expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent('Games');
    // The two the screen has now. "What this screen will show" was the third
    // and went with the work it was waiting on (issue #245); a screen whose
    // only heading is its title gives a screen-reader user nothing to navigate
    // between, which is what this guards.
    expect(
      screen.getAllByRole('heading', { level: 2 }).map((heading) => heading.textContent),
    ).toEqual(expect.arrayContaining(['Every game Clipped knows', 'What has been recorded']));
  });
});

/**
 * The table SPEC.md section 17 asks for, which this screen owed for as long as
 * it named issue #55 as the thing that would land it.
 *
 * #55 closed, `library_games` has carried these figures to this window since,
 * and `useGames` — whose own documentation says "the figures on the Games
 * screen" — was used by Home and by the per-game settings and not by the screen
 * it was named for. These cases are what stops that happening again quietly.
 */
describe('the Games screen, listing what has been recorded', () => {
  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
  });

  const TWO_GAMES = [
    {
      game_id: 'counter-strike-2',
      name: 'Counter-Strike 2',
      sessions: 3,
      recordings: 5,
      clips: 2,
      favourites: 1,
      bytes: 1_500_000_000,
      missing: 0,
      last_played_at: '2026-08-14T09:51:11+01:00',
    },
    // The row for sittings the catalogue would not attribute: no identifier and
    // no name, at most one, last. Drawn rather than hidden — those are
    // recordings somebody made.
    {
      sessions: 1,
      recordings: 1,
      clips: 0,
      favourites: 0,
      bytes: 2_400_000,
      missing: 1,
    },
  ];

  it('draws a row for each game, with what it has recorded of it', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime(
      { link: 'attached', recorder_process_id: 7, features: [], status: { state: 'idle' } },
      null,
      { games: () => Promise.resolve(TWO_GAMES) },
    );
    renderApp();
    await openGames(user);

    const table = await screen.findByRole('table', { name: 'Games recorded' });
    const rows = within(table).getAllByRole('row').slice(1);
    expect(rows).toHaveLength(2);

    const first = within(rows[0] as HTMLElement)
      .getAllByRole('cell')
      .map((cell) => cell.textContent);
    expect(first[0]).toBe('Counter-Strike 2');
    expect(first).toContain('3');
    expect(first).toContain('5');

    // The unattributed row is named rather than blank: a cell with nothing in
    // it reads as a game whose name was lost.
    const second = within(rows[1] as HTMLElement).getAllByRole('cell')[0]?.textContent;
    expect(second).toBe('Not recognised');
  });

  it('says the library holds no games rather than drawing an empty table', async () => {
    // SPEC.md section 6 asks for an empty state that reflects the truth rather
    // than sample data. An empty table under those headings is
    // indistinguishable from a library that could not be read.
    const user = userEvent.setup();
    stubRecorderLinkRuntime(
      { link: 'attached', recorder_process_id: 7, features: [], status: { state: 'idle' } },
      null,
      { games: () => Promise.resolve([]) },
    );
    renderApp();
    await openGames(user);

    expect(await screen.findByText(/holds no games yet/i)).toBeVisible();
    expect(screen.queryByRole('table', { name: 'Games recorded' })).toBeNull();
  });

  it('says why when the library cannot be read, rather than showing nothing', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime(
      { link: 'attached', recorder_process_id: 7, features: [], status: { state: 'idle' } },
      null,
      { games: () => Promise.reject(new Error('the library is locked')) },
    );
    renderApp();
    await openGames(user);

    await waitFor(() => {
      expect(screen.queryByRole('table', { name: 'Games recorded' })).toBeNull();
    });
    // The screen still stands: the detection block is unaffected by a library
    // that would not open, and the failure is stated rather than left as an
    // absence. Asserted on the detection heading rather than on the table this
    // case used to look for, which went with the work it was waiting on.
    expect(screen.getByRole('region', { name: 'Game detection' })).toBeVisible();
    expect(screen.getByText(/locked/i)).toBeVisible();
  });
});

/**
 * The catalogue, which this screen could not list until the protocol could be
 * asked (issue #245).
 *
 * Two things are worth guarding. That the table is drawn from what the recorder
 * answered rather than from anything this window knows — the catalogue is half
 * compiled into the recorder and half a file this process has no permission to
 * open, so a screen that invented a row would be inventing the whole thing. And
 * that a recorder which cannot be asked says so, rather than showing an empty
 * table: "this recorder is older than this window" and "you have no games" are
 * different sentences and only one of them is ever true here.
 */
describe('the Games screen, listing what Clipped knows', () => {
  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
  });

  const CATALOGUE = [
    {
      game_id: 'counter-strike-2',
      name: 'Counter-Strike 2',
      source: 'shipped',
      executables: [
        { name: 'cs2.exe', path_contains: 'steamapps/common/Counter-Strike Global Offensive' },
      ],
      launcher: 'steam',
      launcher_app_id: '730',
      excluded: false,
    },
    {
      game_id: 'a-game-of-my-own',
      name: 'A game of my own',
      source: 'user',
      executables: [{ name: 'mygame.exe' }],
      excluded: true,
    },
  ];

  function attached(features: readonly string[]) {
    return {
      link: 'attached' as const,
      recorder_process_id: 7,
      features,
      status: { state: 'idle' as const },
    };
  }

  it('lists what the recorder answered, saying which entries are the user’s own', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime(attached(['catalogue']), null, {
      catalogue: () => Promise.resolve(CATALOGUE),
    });
    renderApp();
    await openGames(user);

    const table = await screen.findByRole('table', { name: 'Games Clipped knows' });
    const rows = within(table).getAllByRole('row').slice(1);
    expect(rows).toHaveLength(2);

    const shipped = within(rows[0] as HTMLElement)
      .getAllByRole('cell')
      .map((cell) => cell.textContent);
    expect(shipped[0]).toContain('Counter-Strike 2');
    expect(shipped[1]).toContain('cs2.exe');
    expect(shipped[2]).toContain('steam');
    expect(shipped[3]).toContain('shipped');

    const mine = within(rows[1] as HTMLElement)
      .getAllByRole('cell')
      .map((cell) => cell.textContent);
    expect(mine[3]).toContain('yours');
    // An exclusion is a decision about an entry rather than the deletion of
    // one, so it is listed and said in words.
    expect(mine[0]).toContain('excluded');
    // An entry with no launcher says so rather than leaving the cell blank,
    // which reads as a launcher whose name was lost.
    expect(mine[2]).toContain('by name only');
  });

  it('says a recorder that cannot be asked is older, rather than showing an empty table', async () => {
    // The feature is asked before the table is drawn. Without this the screen
    // would send a command the recorder refuses and then report a failed read,
    // and "your catalogue could not be read" is not what happened.
    const user = userEvent.setup();
    stubRecorderLinkRuntime(attached([]), null, {
      catalogue: () => Promise.resolve(CATALOGUE),
    });
    renderApp();
    await openGames(user);

    expect(await screen.findByText(/cannot list its catalogue/i)).toBeVisible();
    expect(screen.queryByRole('table', { name: 'Games Clipped knows' })).toBeNull();
  });

  /**
   * The catalogue as it stands after an edit, which is what the recorder
   * answers with.
   *
   * Deliberately *not* the fixture with one field flipped. The screen must draw
   * what came back rather than what it guessed the edit would do, and a stub
   * that returned the obvious answer could not tell the two apart. So the reply
   * below also renames the entry — something no optimistic update would ever
   * produce — and the assertion is on that name.
   */
  const AFTER_THE_EDIT = [
    CATALOGUE[0],
    {
      ...CATALOGUE[1],
      name: 'What the recorder called it',
      excluded: false,
    },
  ];

  it('draws no controls when the recorder can list the catalogue and not change it', async () => {
    // The two capabilities are separate, and every build between issue #245's
    // read half and its write half is exactly this: it lists and refuses every
    // change. Inferring the controls from the table would draw buttons that
    // answer `unknown_command` (AGENTS.md section 27).
    const user = userEvent.setup();
    stubRecorderLinkRuntime(attached(['catalogue']), null, {
      catalogue: () => Promise.resolve(CATALOGUE),
    });
    renderApp();
    await openGames(user);

    await screen.findByRole('table', { name: 'Games Clipped knows' });
    expect(screen.queryByRole('button', { name: 'Exclude' })).toBeNull();
    expect(screen.queryByRole('button', { name: 'Rename' })).toBeNull();
    expect(screen.queryByRole('form', { name: 'Register a game' })).toBeNull();
    // And says so once, rather than leaving somebody looking for controls that
    // are not coming.
    expect(screen.getByText(/list its catalogue and not change it/i)).toBeVisible();
  });

  it('excludes a game and draws what the recorder answered, not what it assumed', async () => {
    const user = userEvent.setup();
    const runtime = stubRecorderLinkRuntime(attached(['catalogue', 'catalogue_editing']), null, {
      catalogue: () => Promise.resolve(CATALOGUE),
      catalogueEdit: () => Promise.resolve({ game_id: 'counter-strike-2', games: AFTER_THE_EDIT }),
    });
    renderApp();
    await openGames(user);

    const table = await screen.findByRole('table', { name: 'Games Clipped knows' });
    const shipped = within(table).getAllByRole('row')[1] as HTMLElement;
    await user.click(within(shipped).getByRole('button', { name: 'Exclude' }));

    const asked = runtime.invocations.filter((call) => call.command === 'set_game_excluded');
    expect(asked).toHaveLength(1);
    expect(asked[0]?.args).toMatchObject({ gameId: 'counter-strike-2', excluded: true });

    // The reply renamed the *other* entry, which nothing about pressing Exclude
    // would have produced. Finding that name is what proves the table is the
    // recorder's answer rather than a guess.
    expect(await screen.findByText('What the recorder called it')).toBeVisible();
  });

  it('offers Include for a game that is already excluded', async () => {
    // The verb is what pressing it does, not the state it is in. A button
    // reading "Excluded" is ambiguous about which way it points, and this is
    // the control that decides whether a game is recorded at all.
    const user = userEvent.setup();
    const runtime = stubRecorderLinkRuntime(attached(['catalogue', 'catalogue_editing']), null, {
      catalogue: () => Promise.resolve(CATALOGUE),
      catalogueEdit: () => Promise.resolve({ game_id: 'a-game-of-my-own', games: AFTER_THE_EDIT }),
    });
    renderApp();
    await openGames(user);

    const table = await screen.findByRole('table', { name: 'Games Clipped knows' });
    const mine = within(table).getAllByRole('row')[2] as HTMLElement;
    await user.click(within(mine).getByRole('button', { name: 'Include' }));

    const asked = runtime.invocations.filter((call) => call.command === 'set_game_excluded');
    expect(asked[0]?.args).toMatchObject({ gameId: 'a-game-of-my-own', excluded: false });
  });

  it('offers Forget only for entries the user added', async () => {
    // Forgetting a game Clipped ships would be undone by the next update, which
    // is why excluding is the operation that lasts. A button that does not last
    // is worse than no button.
    const user = userEvent.setup();
    stubRecorderLinkRuntime(attached(['catalogue', 'catalogue_editing']), null, {
      catalogue: () => Promise.resolve(CATALOGUE),
    });
    renderApp();
    await openGames(user);

    const table = await screen.findByRole('table', { name: 'Games Clipped knows' });
    const rows = within(table).getAllByRole('row');
    expect(within(rows[1] as HTMLElement).queryByRole('button', { name: 'Forget' })).toBeNull();
    expect(within(rows[2] as HTMLElement).getByRole('button', { name: 'Forget' })).toBeVisible();
  });

  it('registers a game, sending the fields and no folder when none was typed', async () => {
    const user = userEvent.setup();
    const runtime = stubRecorderLinkRuntime(attached(['catalogue', 'catalogue_editing']), null, {
      catalogue: () => Promise.resolve(CATALOGUE),
      catalogueEdit: () => Promise.resolve({ game_id: 'a-new-game', games: AFTER_THE_EDIT }),
    });
    renderApp();
    await openGames(user);

    const form = await screen.findByRole('form', { name: 'Register a game' });
    await user.type(within(form).getByLabelText('Game'), 'A new game');
    await user.type(within(form).getByLabelText('Executable'), 'anew.exe');
    await user.click(within(form).getByRole('button', { name: 'Add game' }));

    const asked = runtime.invocations.filter((call) => call.command === 'register_game');
    expect(asked).toHaveLength(1);
    // An empty box is not a qualifier. `Some("")` would be an entry that
    // matches no path at all, which is worse than the entry not existing.
    expect(asked[0]?.args).toMatchObject({
      name: 'A new game',
      executable: 'anew.exe',
      pathContains: null,
    });
  });

  it('will not register a game with no name or no executable', async () => {
    // Both are required by the entry the recorder would write, so a button that
    // sent an incomplete one would be a button whose command is refused.
    const user = userEvent.setup();
    const runtime = stubRecorderLinkRuntime(attached(['catalogue', 'catalogue_editing']), null, {
      catalogue: () => Promise.resolve(CATALOGUE),
    });
    renderApp();
    await openGames(user);

    const form = await screen.findByRole('form', { name: 'Register a game' });
    const add = within(form).getByRole('button', { name: 'Add game' });
    expect(add).toBeDisabled();

    await user.type(within(form).getByLabelText('Game'), 'A new game');
    expect(add).toBeDisabled();

    await user.type(within(form).getByLabelText('Executable'), 'anew.exe');
    expect(add).toBeEnabled();

    expect(runtime.invocations.filter((call) => call.command === 'register_game')).toHaveLength(0);
  });

  it('clears a rename when the name is emptied, rather than calling the game nothing', async () => {
    const user = userEvent.setup();
    const runtime = stubRecorderLinkRuntime(attached(['catalogue', 'catalogue_editing']), null, {
      catalogue: () => Promise.resolve(CATALOGUE),
      catalogueEdit: () => Promise.resolve({ game_id: 'counter-strike-2', games: AFTER_THE_EDIT }),
    });
    renderApp();
    await openGames(user);

    const table = await screen.findByRole('table', { name: 'Games Clipped knows' });
    const shipped = within(table).getAllByRole('row')[1] as HTMLElement;
    await user.click(within(shipped).getByRole('button', { name: 'Rename' }));

    await user.clear(screen.getByLabelText(/Name for Counter-Strike 2/i));
    await user.click(screen.getByRole('button', { name: 'Save' }));

    const asked = runtime.invocations.filter((call) => call.command === 'rename_game');
    expect(asked).toHaveLength(1);
    expect(asked[0]?.args).toMatchObject({ gameId: 'counter-strike-2', name: null });
  });

  it('says why when an edit does not happen', async () => {
    // An edit that failed silently is somebody believing they excluded a game
    // that is still being recorded, which is what this screen exists to stop.
    const user = userEvent.setup();
    stubRecorderLinkRuntime(attached(['catalogue', 'catalogue_editing']), null, {
      catalogue: () => Promise.resolve(CATALOGUE),
      catalogueEdit: () => Promise.reject(new Error('the games file could not be written')),
    });
    renderApp();
    await openGames(user);

    const table = await screen.findByRole('table', { name: 'Games Clipped knows' });
    const shipped = within(table).getAllByRole('row')[1] as HTMLElement;
    await user.click(within(shipped).getByRole('button', { name: 'Exclude' }));

    expect(await screen.findByRole('alert')).toHaveTextContent(/could not be written/i);
    // And the row is unchanged, rather than showing a state the recorder never
    // reached.
    expect(within(shipped).getByRole('button', { name: 'Exclude' })).toBeVisible();
  });

  it('says why when the catalogue cannot be read', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime(attached(['catalogue']), null, {
      catalogue: () => Promise.reject(new Error('the overlay is not valid TOML')),
    });
    renderApp();
    await openGames(user);

    await waitFor(() => {
      expect(screen.queryByRole('table', { name: 'Games Clipped knows' })).toBeNull();
    });
    expect(screen.queryByText(/cannot list its catalogue/i)).toBeNull();
  });
});
