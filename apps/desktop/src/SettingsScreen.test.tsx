import { cleanup, render, screen, within } from '@testing-library/react';
import userEvent, { type UserEvent } from '@testing-library/user-event';
import { StrictMode } from 'react';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { App } from './App';
import { SETTINGS_SECTIONS } from './settings';
import { SettingsScreen } from './SettingsScreen';
import { stubRecorderLinkRuntime } from './test/recorderLinkRuntime';

/**
 * The Settings screen's contract, as tests (issue #51).
 *
 * Three properties, and each of them is one that would rot in silence.
 *
 * The first is that the screen offers **nothing that would change a setting**.
 * This window cannot read or write one (`settings.ts` says why), so a field, a
 * switch or a Save button here would be the control that silently does nothing
 * of AGENTS.md section 27. The case asserts the absence of anything operable
 * that is not the rail, rather than the presence of an explanation for it, so a
 * later change that adds a control has to come past it.
 *
 * The second is that the screen says what a user can do **instead**, per
 * setting. A screen that only listed what is missing would leave somebody who
 * came to pick a microphone with nothing at all (AGENTS.md section 45), and the
 * list of what each row must name is written out here rather than mapped from
 * the screen's own tables — a case that walked the rendered rows and checked
 * their shape is satisfied by rows somebody invented.
 *
 * The third is that the rail works from the keyboard alone. It is the only
 * control on the screen, and a rail that needed a pointer would put five of the
 * six sections out of reach (AGENTS.md section 46).
 *
 * What is *not* here is whether the settings named are the real ones and the
 * commands named are real commands. Neither can be established from TypeScript:
 * both live in Rust, and `settingsConformance.test.ts` reads them there.
 */

/** Mounts the application the way `main.tsx` does, StrictMode and all. */
function renderApp(): void {
  render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
}

/** Opens one of the rail's sections. */
async function openSection(user: UserEvent, label: string): Promise<void> {
  await user.click(screen.getByRole('tab', { name: label }));
}

/** The pane on show, whichever section opened it. */
function pane(): HTMLElement {
  return screen.getByRole('tabpanel');
}

/** The cells of the row for one setting, in the pane on show. */
function rowFor(label: string): readonly string[] {
  const heading = within(pane()).getByRole('rowheader', { name: new RegExp(`^${label}`) });
  const row = heading.closest('tr');
  if (row === null) {
    throw new Error(`the "${label}" row header is not in a row`);
  }
  return [
    heading.textContent ?? '',
    ...within(row)
      .getAllByRole('cell')
      .map((cell) => cell.textContent ?? ''),
  ];
}

describe('the Settings screen', () => {
  beforeEach(() => {
    window.location.hash = '';
  });

  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
    window.location.hash = '';
  });

  it('says that no setting can be changed here, and why', () => {
    render(<SettingsScreen />);

    const panel = screen.getByRole('region', { name: 'Why nothing here can be changed' });

    expect(within(panel).getByRole('heading', { level: 2 })).toHaveTextContent(
      /^No setting can be changed from this window$/,
    );
    // The mechanism, not an apology: the command that is refused, the file it
    // would have written, and the issue that makes it reachable.
    expect(panel).toHaveTextContent(/apply_settings/);
    expect(panel).toHaveTextContent(/not implemented/);
    expect(panel).toHaveTextContent(/%LOCALAPPDATA%\\Clipped\\settings\.json/);
    expect(panel).toHaveTextContent(/#252/);
  });

  /*
   * The deck draws device pickers, a directory chooser, quality presets and a
   * row of switches. None of them has anything behind it in this build, so none
   * is drawn — not even disabled, because a screen has room to say why and a
   * disabled control says less than the row that names the issue.
   *
   * Asserted as a property rather than as a list of the controls that are
   * absent: anything operable here would have to do something, and the only
   * thing that can is the rail.
   */
  it('offers no control that would change a setting', () => {
    render(<SettingsScreen />);

    // Not vacuous: the rail is here, and it is what the exclusion below is for.
    expect(screen.getAllByRole('tab')).toHaveLength(SETTINGS_SECTIONS.length);

    for (const role of [
      'button',
      'link',
      'textbox',
      'combobox',
      'checkbox',
      'radio',
      'switch',
      'slider',
      'spinbutton',
      'menuitem',
    ] as const) {
      expect(screen.queryAllByRole(role), `nothing on this screen is a ${role}`).toHaveLength(0);
    }
  });

  it('opens each section from the rail, and says which section a pane is', async () => {
    const user = userEvent.setup();
    render(<SettingsScreen />);

    for (const section of SETTINGS_SECTIONS) {
      await openSection(user, section.label);

      expect(screen.getByRole('tab', { name: section.label })).toHaveAttribute(
        'aria-selected',
        'true',
      );
      expect(pane()).toHaveAccessibleName(section.label);
      expect(within(pane()).getByRole('heading', { level: 2 })).toHaveTextContent(section.label);
    }
  });

  /*
   * The rail is one stop in the tab order and the arrow keys move within it,
   * which is the WAI-ARIA tab-list contract. A rail of six buttons each taking
   * a tab stop would put five stops between the sidebar and the pane, every
   * time.
   */
  it('is driven by the keyboard alone: one tab stop, then the arrow keys', async () => {
    const user = userEvent.setup();
    render(<SettingsScreen />);

    const [first, second] = SETTINGS_SECTIONS;
    if (first === undefined || second === undefined) {
      throw new Error('the screen needs at least two sections for this case to mean anything');
    }

    await user.tab();
    expect(screen.getByRole('tab', { name: first.label })).toHaveFocus();

    await user.keyboard('{ArrowDown}');
    expect(screen.getByRole('tab', { name: second.label })).toHaveFocus();
    expect(pane()).toHaveAccessibleName(second.label);

    await user.keyboard('{End}');
    const last = SETTINGS_SECTIONS[SETTINGS_SECTIONS.length - 1];
    expect(pane()).toHaveAccessibleName(last?.label ?? '');

    await user.keyboard('{Home}');
    expect(pane()).toHaveAccessibleName(first.label);

    // And out of the rail in one press, into the pane it opened, rather than
    // through the five sections it did not.
    await user.tab();
    expect(pane()).toHaveFocus();
  });

  it('is reached from the sidebar with Tab and Enter', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime({ link: 'connecting' });
    renderApp();

    // Skip link, Home, Library, Games, Editor, Settings.
    for (let step = 0; step < 6; step += 1) {
      await user.tab();
    }
    expect(document.activeElement).toHaveTextContent('Settings');

    await user.keyboard('{Enter}');

    expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent('Settings');
    expect(screen.getByRole('main')).toHaveFocus();
    expect(screen.getByRole('tablist', { name: 'Settings sections' })).toBeVisible();
  });

  it('heads each column with the question the row answers', () => {
    render(<SettingsScreen />);

    const headers = within(screen.getByRole('table'))
      .getAllByRole('columnheader')
      .map((header) => header.textContent);

    expect(headers).toEqual(['Setting', 'How it is set today', 'What this window needs first']);
  });

  /*
   * What each of these settings is set by today, and the work that has to land
   * before this window can hold it — written out here rather than read from
   * `SETTINGS_SECTIONS`.
   *
   * That is the whole of the case. A test that walked the screen's own rows and
   * asserted "the third cell names some issue" is satisfied by a table of rows
   * somebody made up, which is exactly what a review of the Games screen
   * demonstrated by replacing every row with `{ shows: 'x', needs: 'Issue #1' }`
   * and watching the suite stay green. The claim is that *these* settings are
   * pinned to *these* pieces of work.
   *
   * Every entry is one a user might come to this screen for, and the two files
   * are named in full because a path is the only thing on this screen anybody
   * can act on directly (AGENTS.md sections 28 and 45).
   */
  const MUST_BE_NAMED: readonly (readonly [
    section: string,
    setting: string,
    today: RegExp,
    issues: readonly number[],
  ])[] = [
    ['Recording', 'Frame rate', /clipped-recorder watch --framerate 60/, [61, 252]],
    ['Recording', 'Replay window', /no build starts a recording that runs one/, [38, 61, 252]],
    ['Recording', 'Recording format', /Matroska/, [307]],
    ['Audio', 'Microphone', /clipped-recorder watch --microphone default/, [180, 308, 252]],
    ['Audio', 'Audio tracks, enable and level', /nothing produces the tracks/, [180, 81, 33]],
    ['Storage', 'Recording directory', /--output-directory/, [307]],
    ['Storage', 'Trash and recovery', /no trash to recover from/, [94]],
    ['Hotkeys', 'Which combination an action has', /Ctrl\+F9 to bookmark/, [54, 233]],
    ['Hotkeys', 'A combination another application owns', /a key that does nothing/, [417]],
    ['Notifications', 'A recording failed', /"recording_failed": false/, [252]],
    [
      'Startup',
      'Start the recorder when I sign in',
      /clipped-recorder start-at-login enable/,
      [308],
    ],
  ];

  it.each(MUST_BE_NAMED)(
    'says how %s > %s is set today, and what would bring it here',
    async (section, setting, today, issues) => {
      const user = userEvent.setup();
      render(<SettingsScreen />);
      await openSection(user, section);

      const [, isSetBy, waitsFor] = rowFor(setting);

      expect(isSetBy ?? '').toMatch(today);
      for (const issue of issues) {
        expect(waitsFor ?? '', `${setting} waits on #${String(issue)}`).toMatch(
          new RegExp(`#${String(issue)}\\b`),
        );
      }
    },
  );

  /*
   * The notification switches are the one thing on this screen somebody can
   * change today, and the way to change them is a file this window does not
   * write. Saying so is the difference between a screen that is honest and one
   * that is merely apologetic.
   */
  it('names the file the notification switches are set in, and their keys', async () => {
    const user = userEvent.setup();
    render(<SettingsScreen />);
    await openSection(user, 'Notifications');

    expect(pane()).toHaveTextContent(/%APPDATA%\\uk\.wildware\.clipped\\notifications\.json/);
    for (const key of ['recording_failed', 'recording_interrupted', 'recorder_unavailable']) {
      expect(pane()).toHaveTextContent(new RegExp(key));
    }
    expect(pane()).toHaveTextContent(/Every category is on until that file says otherwise/);
  });

  /*
   * Every row, not only the ten above: a row that named no work at all would be
   * a statement that something is missing with nothing behind it, which is the
   * shape of a promise nobody has made.
   */
  it('pins every setting to the work that would bring it here', () => {
    for (const section of SETTINGS_SECTIONS) {
      for (const row of section.rows) {
        expect(row.needs, `${section.label} > ${row.label}`).toMatch(/#\d+/);
        expect(row.today.length, `${section.label} > ${row.label}`).toBeGreaterThan(0);
      }
    }
  });

  it('has a heading for the panel and for the section on show', () => {
    render(<SettingsScreen />);

    expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent('Settings');
    expect(
      screen.getAllByRole('heading', { level: 2 }).map((heading) => heading.textContent),
    ).toEqual(['No setting can be changed from this window', 'Recording']);
  });
});
