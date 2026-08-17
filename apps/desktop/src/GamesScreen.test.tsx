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
   * The screen's one table is what it cannot show and why, not a list of games.
   * An empty Game / Recording / Last played table is indistinguishable from a
   * machine that has played nothing, and this build has not looked.
   */
  it('draws a table of what is missing rather than an empty table of games', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime({
      link: 'attached',
      recorder_process_id: 7,
      features: [],
      status: { state: 'idle' },
    });
    renderApp();
    await openGames(user);

    const headers = within(screen.getByRole('table'))
      .getAllByRole('columnheader')
      .map((header) => header.textContent);
    expect(headers).toEqual(['What the Games screen will show', 'What has to exist first']);
  });

  /*
   * What SPEC.md sections 6 and 17 ask this screen for, and the issue that
   * supplies each — written out here rather than mapped from the screen's own
   * `MISSING` array.
   *
   * That distinction is the whole test. A case that walked the rendered rows
   * and asserted "two cells, and the second names some issue" is satisfied by a
   * table holding one invented row, which is what a review demonstrated by
   * replacing all four with `{ shows: 'x', needs: 'Issue #1' }` and watching the
   * suite stay green. The claim being made is not "the rows have the right
   * shape", it is "these four things are the four this screen owes and each is
   * pinned to the work that lands it", and only a list kept independently of the
   * implementation can say that.
   *
   * Adding a fifth row is a new promise to the reader, so it belongs here too;
   * the count is asserted for that reason rather than for tidiness.
   */
  const MUST_BE_NAMED: readonly (readonly [string, RegExp, readonly number[]])[] = [
    ['the catalogue of games', /every game Clipped knows/i, [245]],
    ['registering, renaming, excluding and disabling', /adding an unknown executable/i, [45, 245]],
    ['counts and storage per game', /sessions, clips, favourites and storage/i, [55]],
    ['what is being recorded right now', /which game is being recorded now/i, [241]],
  ];

  it('names each thing it owes, and the issue that lands it', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime({
      link: 'attached',
      recorder_process_id: 7,
      features: [],
      status: { state: 'idle' },
    });
    renderApp();
    await openGames(user);

    const rows = within(screen.getByRole('table')).getAllByRole('row').slice(1);
    expect(rows).toHaveLength(MUST_BE_NAMED.length);

    for (const [subject, shows, issues] of MUST_BE_NAMED) {
      const matching = rows.filter((row) =>
        shows.test(within(row).getAllByRole('cell')[0]?.textContent ?? ''),
      );
      expect(matching, `one row for ${subject}`).toHaveLength(1);

      const needs = within(matching[0] as HTMLElement).getAllByRole('cell')[1]?.textContent ?? '';
      for (const issue of issues) {
        expect(needs, `${subject} is waiting on #${String(issue)}`).toMatch(
          new RegExp(`#${String(issue)}\\b`),
        );
      }
    }
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
  it('has a heading for each of its two parts', () => {
    render(<GamesScreen link={null} />);

    expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent('Games');
    expect(
      screen.getAllByRole('heading', { level: 2 }).map((heading) => heading.textContent),
    ).toContain('What this screen will show');
  });
});
