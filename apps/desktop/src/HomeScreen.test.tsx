import { cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { StrictMode } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { App } from './App';
import { HomeScreen } from './HomeScreen';
import { describeRecordingNow } from './recordingNow';
import { A_COUNT, textRuns } from './test/counts';
import { stubRecorderLinkRuntime } from './test/recorderLinkRuntime';
import type { RecorderLinkState } from './useRecorderLink';

/**
 * The Home screen's contract, as tests (issue #60).
 *
 * SPEC.md section 17 draws Home as recent sessions, recently clipped,
 * favourites and games. Not one of those can be read from this window — the
 * library index lives in the recorder's process and no protocol command reads it
 * (issue #301) — so the properties worth guarding are about what is *not* drawn
 * as much as what is.
 *
 * Four of them:
 *
 * - **no figure is invented.** No count, no total, no "0 sessions". A zeroed
 *   tile is indistinguishable from a machine that has recorded nothing, and this
 *   build has not looked (AGENTS.md section 27).
 * - **no wording claims more than the link can establish.** The link sees the
 *   one recorder this window started or attached to; a `clipped-recorder watch`
 *   in a terminal is invisible to it. So "Nothing is being recorded" is a claim
 *   about the machine that nothing here measured, and the heading has to name
 *   what it speaks for.
 * - **the one real thing reaches the screen, and follows the link.** A screen
 *   whose wording is a constant looks identical to one that is following the
 *   recorder, which is why the case below drives the whole application and moves
 *   the link underneath it.
 * - **no duration.** `elapsed_ms` is on the wire and is measured at the moment
 *   the recording started; the recorder publishes no status between start and
 *   end, so a duration drawn from it would be frozen at zero for ever.
 */

/** Mounts the application the way `main.tsx` does, StrictMode and all. */
function renderApp(): void {
  render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}

/** The live region the recording state is drawn in. */
function recordingPanel(): HTMLElement {
  return screen.getByRole('region', { name: 'Recording now' });
}

/** A recorder attached and writing a file, with a long elapsed time on it. */
const RECORDING: RecorderLinkState = {
  link: 'attached',
  recorder_process_id: 7,
  status: {
    state: 'recording',
    recording_id: 'rec-1',
    output: 'D:\\clips\\clipped-cs2-2026-08-11T21-04-19.mkv',
    target: 'process cs2.exe',
    elapsed_ms: 754_000,
  },
};

/*
 * Every state the link has, and what each one means for "what is being recorded
 * now". The five are listed rather than generated: the failure this guards
 * against is two states collapsing into one wording, and a table built from the
 * states themselves could not see that happen.
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
    'when a recorder is attached and idle',
    { link: 'attached', recorder_process_id: 7, status: { state: 'idle' } },
    'This recorder is not recording',
    /running and idle.*clipped-recorder watch.*invisible here/,
  ],
  ['when a recording is running', RECORDING, 'Recording process cs2.exe', /being written now/],
];

describe('what the Home screen says about the recording now', () => {
  it.each(STATES)('is described %s', (_case, link, state, detail) => {
    const described = describeRecordingNow(link);

    expect(described.state).toBe(state);
    expect(described.detail).toMatch(detail);
  });

  /*
   * The heading is the part a screen-reader user skips to and the part somebody
   * takes away, so it has to carry its own scope. Three forms are allowed and
   * no fourth: "Not known…", which claims nothing; "This recorder…", which names
   * what the claim is about; and "Recording <target>", which asserts a recording
   * that demonstrably exists and is therefore true whatever else is running.
   *
   * The wording this rules out is "Nothing is being recorded", which reads as a
   * statement about the machine. `clipped-recorder watch` serves no protocol and
   * is invisible to this link, so it could be recording a game while this window
   * said so — and the screen's own next paragraph says as much.
   *
   * Asserted over all six renderings rather than over the one that would be
   * wrong, because the defect is a class: it is the same unscoped heading
   * whichever branch it gets written into.
   */
  it.each(STATES)('either names what it speaks for, or claims nothing, %s', (_case, link) => {
    expect(describeRecordingNow(link).state).toMatch(/^(Not known|This recorder |Recording )/);
  });

  /*
   * The file is the only thing on this screen anybody can act on, and it is only
   * ever the one the recorder says it is writing. A path invented, guessed at or
   * left over from a previous recording would put somebody else's footage in
   * front of the user.
   */
  it('names the file being written, and only when one is', () => {
    expect(describeRecordingNow(RECORDING).output).toBe(
      'D:\\clips\\clipped-cs2-2026-08-11T21-04-19.mkv',
    );

    for (const [, link] of STATES.filter(([, candidate]) => candidate !== RECORDING)) {
      expect(describeRecordingNow(link).output).toBeUndefined();
    }
  });

  /*
   * `elapsed_ms` arrives with the status and is 754,000 in the fixture above.
   * The recorder publishes `status_changed` when a recording starts and when it
   * ends and at no point between (`apps/recorder/src/serve.rs`), so a duration
   * drawn from it is the duration at the moment the recording started and never
   * moves. Counting up from it in the window would be a figure nobody measured.
   *
   * The assertion is on the shape of a duration rather than on the number,
   * because "12:34", "754 s" and "12 minutes" are the same mistake.
   */
  it('shows no duration, because the one on the wire never moves', () => {
    const described = describeRecordingNow(RECORDING);
    const said = `${described.state} ${described.detail} ${described.output ?? ''}`;

    expect(said).not.toMatch(/\d+:\d\d\b/);
    expect(said).not.toMatch(/\b\d+\s*(?:ms|s|sec|secs|seconds|m|min|mins|minutes|h|hours)\b/i);
    expect(said).not.toMatch(/elapsed|so far|running for/i);
  });
});

describe('the Home screen', () => {
  beforeEach(() => {
    window.location.hash = '';
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
    window.location.hash = '';
  });

  /*
   * The property is that the screen shows the recorder's real state rather than
   * a sentence somebody typed, so the case drives the whole application and
   * moves the link underneath it. A screen whose wording is a constant looks
   * identical to one that is following the link, and stays identical after the
   * link has been disconnected from it.
   */
  it('follows the recorder link rather than showing a sentence that was typed once', async () => {
    const runtime = stubRecorderLinkRuntime({ link: 'connecting' });
    renderApp();

    // Anchored, because "Not known" is a substring of "Not known yet" and an
    // unanchored match would let the screen sit on one wording for both.
    await waitFor(() => {
      expect(within(recordingPanel()).getByRole('heading', { level: 2 })).toHaveTextContent(
        /^Not known yet$/,
      );
    });

    runtime.emit({ event: 'state', ...RECORDING });

    await waitFor(() => {
      expect(within(recordingPanel()).getByRole('heading', { level: 2 })).toHaveTextContent(
        /^Recording process cs2\.exe$/,
      );
    });
    expect(
      within(recordingPanel()).getByText('D:\\clips\\clipped-cs2-2026-08-11T21-04-19.mkv'),
    ).toBeVisible();

    // And back to idle, so the case covers a move in both directions and the
    // file going away with the recording rather than staying on screen.
    runtime.emit({
      event: 'state',
      link: 'attached',
      recorder_process_id: 7,
      status: { state: 'idle' },
    });

    await waitFor(() => {
      expect(within(recordingPanel()).getByRole('heading', { level: 2 })).toHaveTextContent(
        /^This recorder is not recording$/,
      );
    });
    expect(
      within(recordingPanel()).queryByText('D:\\clips\\clipped-cs2-2026-08-11T21-04-19.mkv'),
    ).toBeNull();
  });

  it('announces a change of state rather than only drawing it', async () => {
    stubRecorderLinkRuntime({ link: 'connecting' });
    renderApp();

    await waitFor(() => {
      expect(recordingPanel()).toHaveAttribute('aria-live', 'polite');
    });
  });

  /*
   * The deck draws Home as tiles of recent sessions, recent clips and
   * favourites, each of which opens something. None of them has anything behind
   * it in this build, so none is drawn. This asserts the property rather than
   * the three cases: anything operable inside the screen would have to do
   * something, and nothing here can.
   */
  it('offers no control, because nothing it would drive can be reached', async () => {
    stubRecorderLinkRuntime(RECORDING);
    renderApp();

    await waitFor(() => {
      expect(recordingPanel()).toBeInTheDocument();
    });

    const main = screen.getByRole('main');
    expect(within(main).queryAllByRole('button')).toHaveLength(0);
    expect(within(main).queryAllByRole('link')).toHaveLength(0);
    expect(within(main).queryAllByRole('textbox')).toHaveLength(0);
    expect(within(main).queryAllByRole('combobox')).toHaveLength(0);
    expect(within(main).queryAllByRole('checkbox')).toHaveLength(0);
    expect(within(main).queryAllByRole('radio')).toHaveLength(0);
  });

  /*
   * The failure this exists for is the tempting one: four tiles reading "0
   * sessions", "0 clips", "0 favourites", "0 B", which look like a finished
   * screen on an empty library and are indistinguishable from one on a full
   * library the window could not read.
   *
   * It matches a bare figure against the nouns SPEC.md section 17 counts, so a
   * count of anything — zero or otherwise — fails until something has actually
   * been measured. Run over each text node rather than over the screen's
   * `textContent`, for the reason `test/counts.ts` sets out.
   */
  it('draws no count, because nothing here has counted anything', async () => {
    stubRecorderLinkRuntime(RECORDING);
    renderApp();

    await waitFor(() => {
      expect(recordingPanel()).toBeInTheDocument();
    });

    for (const run of textRuns(screen.getByRole('main'))) {
      expect(run).not.toMatch(A_COUNT);
    }
  });

  /*
   * What SPEC.md section 17 asks of Home, and the issue that supplies each —
   * written out here rather than mapped from the screen's own array.
   *
   * That distinction is the whole test. A case that walked the rendered rows and
   * asserted "two cells, and the second names some issue" is satisfied by a
   * table holding one invented row. The claim is not "the rows have the right
   * shape", it is "these four are the four this screen owes, and each is pinned
   * to the work that lands it".
   */
  const MUST_BE_NAMED: readonly (readonly [string, RegExp, readonly number[]])[] = [
    ['recent sessions', /recent sessions/i, [56, 301]],
    ['recently clipped', /recently clipped/i, [74, 301]],
    ['favourites', /^favourites/i, [58, 301]],
    ['games, with counts and storage', /games, with the sessions/i, [301, 107]],
  ];

  it('names each list it owes, and the issue that lands it', async () => {
    stubRecorderLinkRuntime({ link: 'connecting' });
    renderApp();

    const rows = within(await screen.findByRole('table'))
      .getAllByRole('row')
      .slice(1);
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
   * Home is the screen the application opens on, so navigating *to* it from a
   * cold start is not a navigation at all — and the shell deliberately does not
   * move focus on the first screen. The path worth checking is coming back to
   * it, which is the one that has to move focus so that a screen reader
   * announces the change.
   *
   * The window therefore opens on Library, by putting the route in the fragment
   * before mounting, exactly as reopening the application on the screen it was
   * last on would. Everything after that is the keyboard alone, because
   * "keyboard navigation" is one of issue #60's three acceptance criteria and a
   * screen only reachable by mouse fails it whatever the shell does.
   */
  it('is reached with Tab and Enter, and takes focus when it opens', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime({ link: 'connecting' });
    window.location.hash = '#/library';
    renderApp();
    expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent('Library');

    // Skip link, Home.
    await user.tab();
    await user.tab();
    expect(document.activeElement).toHaveTextContent('Home');

    await user.keyboard('{Enter}');

    expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent('Home');
    expect(screen.getByRole('main')).toHaveFocus();
  });

  /*
   * Rendered on its own rather than through the shell, so that the screen's own
   * heading structure is the subject: a screen whose only heading was its title
   * gives a screen-reader user nothing to navigate between.
   */
  it('has a heading for each of its two parts', () => {
    render(<HomeScreen link={null} />);

    expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent('Home');
    expect(
      screen.getAllByRole('heading', { level: 2 }).map((heading) => heading.textContent),
    ).toContain('What this screen will show');
  });
});
