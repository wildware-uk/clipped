import type { SettingEntry, SettingsView } from '@clipped/shared';
import { cleanup, render, screen, waitFor, within } from '@testing-library/react';
import userEvent, { type UserEvent } from '@testing-library/user-event';
import { StrictMode } from 'react';
import { MemoryRouter } from 'react-router';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { App } from './App';
import { SETTINGS_SECTIONS } from './settings';
import { SettingsScreen } from './SettingsScreen';
import { stubRecorderLinkRuntime, type StubbedRuntime } from './test/recorderLinkRuntime';

/**
 * The Settings screen's contract, as tests (issue #51).
 *
 * The properties, and each of them is one that would rot in silence.
 *
 * The first is the **MVP path**: somebody opens this screen, picks a microphone
 * out of the machine's own list and a folder to record into, and both reach the
 * recorder as a change to the settings file it owns (SPEC.md section 45, step
 * 3). It is asserted against what the window sent, because the whole point of
 * this screen is that something arrives at the other end.
 *
 * The second is that a setting **nothing reads** is not drawn as a control. The
 * recorder says which those are, and a screen that drew one as a working field
 * would be the control that silently does nothing of AGENTS.md section 27 — the
 * exact failure this screen was written twice to avoid.
 *
 * The third is that a **refusal** arrives as the recorder's own sentence, over
 * the value that was refused, with what was typed still on screen to correct
 * (AGENTS.md section 45).
 *
 * The fourth is that the rail works from the keyboard alone, because it is how
 * five of the six sections are reached (AGENTS.md section 46).
 *
 * What is *not* here is whether the settings named are the real ones and the
 * commands named are real commands. Neither can be established from TypeScript:
 * both live in Rust, and `settingsConformance.test.ts` reads them there.
 */

/** One setting as the recorder sends it, with the fields a control needs. */
function entry(overrides: Partial<SettingEntry> & Pick<SettingEntry, 'key'>): SettingEntry {
  return {
    label: overrides.key,
    value: '',
    overridden: false,
    accepted: 'something',
    applies: true,
    ...overrides,
  };
}

/** The settings a recorder with nothing configured sends. */
const SETTINGS: SettingsView = {
  file: String.raw`C:\Users\alex\AppData\Local\Clipped\settings.json`,
  settings: [
    entry({
      key: 'capture_target',
      label: 'Capture target',
      value: 'game-window',
      choices: ['game-window', 'display'],
      accepted: '"game-window" or "display"',
      // The row this screen has to get right: a key the file carries and no
      // recording reads.
      applies: false,
      unavailable: 'every recording captures the game’s own window (issue #61)',
    }),
    entry({
      key: 'framerate',
      label: 'Frame rate',
      value: '60',
      accepted: '1-480 frames per second',
    }),
    entry({
      key: 'codec',
      label: 'Codec',
      value: 'auto',
      choices: ['auto', 'h264', 'hevc', 'av1'],
      accepted: '"auto", "h264", "hevc" or "av1"',
    }),
    entry({
      key: 'microphone',
      label: 'Microphone',
      value: 'default',
      accepted: '"default", "none" or a device name',
    }),
    entry({
      key: 'recording_directory',
      label: 'Recording directory',
      value: String.raw`C:\Users\alex\Videos\Clipped`,
      accepted: 'a folder on this machine, such as D:\\Clips',
    }),
    // The settings this window itself acts on, and the only booleans on the
    // screen: a closed set of `true` and `false` is what makes one a switch
    // rather than a two-option dropdown (issue #252).
    entry({
      key: 'recording_failed',
      label: 'A recording failed',
      value: 'true',
      choices: ['true', 'false'],
      accepted: 'true or false',
    }),
    entry({
      key: 'recording_interrupted',
      label: 'A recording was interrupted',
      value: 'true',
      choices: ['true', 'false'],
      accepted: 'true or false',
    }),
    entry({
      key: 'recorder_unavailable',
      label: 'The recorder cannot be reached',
      value: 'true',
      choices: ['true', 'false'],
      accepted: 'true or false',
    }),
    entry({
      key: 'hotkey_unavailable',
      label: 'A hotkey is unavailable',
      value: 'true',
      choices: ['true', 'false'],
      accepted: 'true or false',
    }),
  ],
};

/** The same, with one setting answered differently. */
function settingsWith(key: string, changes: Partial<SettingEntry>): SettingsView {
  return {
    ...SETTINGS,
    settings: SETTINGS.settings.map((candidate) =>
      candidate.key === key ? { ...candidate, ...changes } : candidate,
    ),
  };
}

/**
 * The settings a profile that has been through the first run sends.
 *
 * Both answers the first run asks for are configured, which is what stops the
 * shell sending the window to the setup flow (`setup.ts`, issue #109). A test
 * about the sidebar has to start from a profile that is past that, or it is a
 * test about the first run.
 */
const CONFIGURED: SettingsView = {
  ...SETTINGS,
  settings: SETTINGS.settings.map((candidate) =>
    candidate.key === 'microphone' || candidate.key === 'recording_directory'
      ? { ...candidate, overridden: true }
      : candidate,
  ),
};

/** This machine's microphones, as `get_audio_devices` answers. */
const MICROPHONES = {
  microphones: [
    { name: 'Realtek Line In', is_default: false },
    { name: 'Shure MV7', is_default: true },
  ],
};

/** A startup arrangement, as `get_start_at_login` answers. */
const RUN_VALUE = String.raw`HKEY_CURRENT_USER\Software\Microsoft\Windows\CurrentVersion\Run\Clipped Recorder`;

/** Nothing arranged: the state every machine is in until somebody asks. */
const NOT_AT_LOGIN = { enabled: false, location: RUN_VALUE };

/** Arranged, and the recorder it names is where it says it is. */
const AT_LOGIN = {
  enabled: true,
  location: RUN_VALUE,
  command: String.raw`"C:\Program Files\Clipped\clipped-recorder.exe" serve --watch-for-games`,
};

/** What the window sent to `set_start_at_login`, in order. */
function switched(runtime: StubbedRuntime): readonly unknown[] {
  return runtime.invocations
    .filter((invocation) => invocation.command === 'set_start_at_login')
    .map((invocation) => invocation.args['enabled']);
}

/** What the window sent to `apply_recorder_settings`, in order. */
function saved(runtime: StubbedRuntime): readonly Record<string, unknown>[] {
  return runtime.invocations
    .filter((invocation) => invocation.command === 'apply_recorder_settings')
    .map((invocation) => invocation.args['values'] as Record<string, unknown>);
}

/** Mounts the screen over a recorder that answers with `SETTINGS`. */
function renderScreen(
  overrides: Parameters<typeof stubRecorderLinkRuntime>[2] = {},
): StubbedRuntime {
  const runtime = stubRecorderLinkRuntime({ link: 'connecting' }, null, {
    recorderSettings: () => SETTINGS,
    audioDevices: () => MICROPHONES,
    recorderHotkeys: () => [],
    ...overrides,
  });
  // In a router because the screen carries one link — the way back to the
  // first run (issue #109) — and `Link` reads the router's context.
  render(
    <MemoryRouter>
      <SettingsScreen />
    </MemoryRouter>,
  );
  return runtime;
}

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

  /*
   * Step 3 of SPEC.md section 45's MVP definition, from this side of the pipe:
   * "select microphone and recording directory". The device comes from the
   * machine's own list rather than from a name somebody has to type, and what
   * is asserted is the change the window sent — a screen that redrew itself
   * beautifully and sent nothing would pass every other case here.
   */
  it('saves the microphone somebody picked out of this machine’s own list', async () => {
    const user = userEvent.setup();
    const runtime = renderScreen({
      applySettings: () =>
        settingsWith('microphone', { value: 'name:Shure MV7', overridden: true }),
    });

    await openSection(user, 'Audio');
    const microphone = await screen.findByLabelText('Microphone');

    // The default one is marked, because `default` follows whichever endpoint
    // Windows is using and somebody choosing has to know which that is.
    expect(within(microphone).getByRole('option', { name: /Shure MV7/ })).toHaveTextContent(
      'Shure MV7 (default)',
    );

    await user.selectOptions(microphone, 'name:Shure MV7');
    await user.click(screen.getByRole('button', { name: 'Save changes' }));

    await waitFor(() => {
      expect(saved(runtime)).toEqual([{ microphone: 'name:Shure MV7' }]);
    });

    // And the screen redraws from what came back rather than from what it sent.
    expect(await screen.findByLabelText('Microphone')).toHaveValue('name:Shure MV7');
    expect(screen.getByRole('button', { name: 'Reset Microphone' })).toBeVisible();
  });

  it('saves the recording directory somebody chose from the folder dialog', async () => {
    const user = userEvent.setup();
    const runtime = renderScreen({
      openDialog: () => String.raw`D:\Clips`,
      applySettings: () =>
        settingsWith('recording_directory', { value: String.raw`D:\Clips`, overridden: true }),
    });

    await openSection(user, 'Storage');
    await screen.findByLabelText('Recording directory');
    await user.click(screen.getByRole('button', { name: 'Browse…' }));

    await waitFor(() => {
      expect(screen.getByLabelText('Recording directory')).toHaveValue(String.raw`D:\Clips`);
    });
    await user.click(screen.getByRole('button', { name: 'Save changes' }));

    await waitFor(() => {
      expect(saved(runtime)).toEqual([{ recording_directory: String.raw`D:\Clips` }]);
    });
  });

  /*
   * Where automatic recordings are written moves between sittings and never
   * during one, so that a sitting’s session record is never separated from the
   * files it names (AGENTS.md section 56, issue #609). For the length of the
   * sitting that is open the folder on screen is therefore the one that was
   * saved and not the one the footage is going to — which, unsaid, is a control
   * that appears to have done nothing (AGENTS.md section 27).
   *
   * The sentence is the recorder’s: only it knows what is in force. What this
   * screen has to get right is drawing it *beside* the control rather than in
   * place of it, since the value is still editable and still what was saved.
   */
  it('says where recordings still go while a saved folder is waiting on a sitting', async () => {
    const user = userEvent.setup();
    renderScreen({
      recorderSettings: () =>
        settingsWith('recording_directory', {
          value: String.raw`D:\Clips`,
          overridden: true,
          not_yet_in_force: String.raw`Automatic recordings still go to C:\Users\alex\Videos\Clipped. They go here from the next session.`,
        }),
    });

    await openSection(user, 'Storage');

    // Still a control, and still holding what was saved: this is not a setting
    // nothing reads, which is the row `unavailable` is for.
    expect(await screen.findByLabelText('Recording directory')).toHaveValue(String.raw`D:\Clips`);
    expect(pane()).toHaveTextContent(
      String.raw`Automatic recordings still go to C:\Users\alex\Videos\Clipped. They go here from the next session.`,
    );
  });

  it('says nothing about waiting when the folder is the one being used', async () => {
    // The other half, and the one that keeps the sentence honest: a recorder
    // with nothing to wait for sends no `not_yet_in_force`, and a screen that
    // invented a delay would be describing one that is not there.
    const user = userEvent.setup();
    renderScreen();

    await openSection(user, 'Storage');
    await screen.findByLabelText('Recording directory');

    expect(pane()).not.toHaveTextContent('Automatic recordings still go to');
  });

  /*
   * Save says "save changes" in front of the section it is in, so that is what
   * it saves. An edit left in another pane is somebody's to come back to, and a
   * button that quietly sent it as well would be doing something it did not say
   * — with a value they had stopped part way through typing.
   */
  it('saves the section it is in, and leaves an edit in another pane alone', async () => {
    const user = userEvent.setup();
    const runtime = renderScreen({
      applySettings: () => settingsWith('microphone', { value: 'none', overridden: true }),
    });

    await openSection(user, 'Recording');
    const framerate = await screen.findByLabelText('Frame rate');
    await user.clear(framerate);
    await user.type(framerate, '90');

    await openSection(user, 'Audio');
    await user.selectOptions(await screen.findByLabelText('Microphone'), 'none');
    await user.click(screen.getByRole('button', { name: 'Save changes' }));

    await waitFor(() => {
      expect(saved(runtime)).toEqual([{ microphone: 'none' }]);
    });

    // And the frame rate is still where it was left, rather than saved or lost.
    await openSection(user, 'Recording');
    expect(screen.getByLabelText('Frame rate')).toHaveValue('90');
  });

  /*
   * The recorder says which settings nothing reads when a recording starts, and
   * a control for one of those is a control that silently does nothing — the
   * defect this screen has been rewritten around twice (AGENTS.md section 27).
   */
  it('draws a setting nothing reads as its value and the reason, never as a control', async () => {
    const user = userEvent.setup();
    renderScreen();

    await openSection(user, 'Recording');
    await screen.findByLabelText('Frame rate');

    expect(screen.queryByLabelText('Capture target')).toBeNull();
    expect(pane()).toHaveTextContent('every recording captures the game’s own window (issue #61)');
    expect(pane()).toHaveTextContent('game-window');
  });

  /*
   * The refusal is the recorder's, word for word: it names the setting, the
   * value and what would have been accepted, and this window invents no wording
   * of its own for a judgement it did not make. What was typed stays, because
   * somebody has to be able to correct it (AGENTS.md section 45).
   */
  it('shows the recorder’s own refusal, and keeps what was typed', async () => {
    const user = userEvent.setup();
    const runtime = renderScreen({
      applySettings: () => {
        throw {
          code: 'invalid_parameters',
          message: '`framerate` cannot be 900: 1-480 frames per second',
        };
      },
    });

    await openSection(user, 'Recording');
    const framerate = await screen.findByLabelText('Frame rate');
    await user.clear(framerate);
    await user.type(framerate, '900');
    await user.click(screen.getByRole('button', { name: 'Save changes' }));

    expect(await screen.findByRole('alert')).toHaveTextContent(
      '`framerate` cannot be 900: 1-480 frames per second',
    );
    expect(screen.getByLabelText('Frame rate')).toHaveValue('900');
    expect(saved(runtime)).toEqual([{ framerate: '900' }]);
  });

  /*
   * Reset is `null`, not "write the default in": a setting reset here follows a
   * later change to the default, and one set to today's default does not
   * (`docs/configuration.md`). Offered only for a setting the recorder says was
   * configured, because Reset on a value nobody set does nothing.
   */
  it('resets a setting by clearing it, and offers Reset only for one that was set', async () => {
    const user = userEvent.setup();
    const runtime = renderScreen({
      recorderSettings: () => settingsWith('framerate', { value: '120', overridden: true }),
      applySettings: () => SETTINGS,
    });

    await openSection(user, 'Recording');
    await screen.findByLabelText('Frame rate');

    expect(screen.queryByRole('button', { name: 'Reset Codec' })).toBeNull();
    await user.click(screen.getByRole('button', { name: 'Reset Frame rate' }));

    await waitFor(() => {
      expect(saved(runtime)).toEqual([{ framerate: null }]);
    });
    expect(await screen.findByLabelText('Frame rate')).toHaveValue('60');
  });

  /*
   * And the list follows the machine. Issue #308 asks for a list that "changes
   * when a device is plugged in or removed", and the recorder enumerates the
   * endpoints on every request so that it can be asked twice — but a window
   * that asked once at mount would offer whatever happened to be plugged in
   * when the screen opened, for as long as it stayed open. Coming back to the
   * window is the moment worth asking again: it is what somebody does after
   * plugging a headset in.
   */
  it('asks for the microphones again when the window comes back to the front', async () => {
    const user = userEvent.setup();
    let plugged = false;
    renderScreen({
      audioDevices: () =>
        plugged
          ? {
              microphones: [...MICROPHONES.microphones, { name: 'Elgato Wave', is_default: false }],
            }
          : MICROPHONES,
    });

    await openSection(user, 'Audio');
    const before = within(await screen.findByLabelText('Microphone')).getAllByRole('option');
    expect(before.map((option) => option.textContent)).not.toContain('Elgato Wave');

    plugged = true;
    window.dispatchEvent(new Event('focus'));

    await waitFor(() => {
      const after = within(screen.getByLabelText('Microphone')).getAllByRole('option');
      expect(after.map((option) => option.textContent)).toContain('Elgato Wave');
    });
  });

  /*
   * A machine whose microphones could not be enumerated is a machine that was
   * asked and could not answer, which is not the same as one with no
   * microphone — and only one of the two means "plug something in".
   */
  it('says the microphones could not be listed rather than showing an empty list', async () => {
    const user = userEvent.setup();
    renderScreen({
      audioDevices: () => {
        throw { code: 'internal', message: 'this machine’s microphones could not be listed' };
      },
    });

    await openSection(user, 'Audio');
    const microphone = await screen.findByLabelText('Microphone');

    expect(within(microphone).getAllByRole('option')).toHaveLength(2);
    expect(pane()).toHaveTextContent('could not be listed');
  });

  /*
   * A recorder that cannot be reached is one whose settings are unknown, and a
   * screen that drew empty controls over it would be offering to change
   * something it never read.
   */
  it('draws no control at all when the recorder cannot be asked', async () => {
    const user = userEvent.setup();
    renderScreen({
      recorderSettings: () => {
        throw { code: 'recorder_unreachable', message: 'the recorder is not running' };
      },
    });

    await openSection(user, 'Recording');
    await screen.findByText(/the recorder is not running/);

    expect(screen.queryAllByRole('textbox')).toHaveLength(0);
    expect(screen.queryAllByRole('combobox')).toHaveLength(0);
    expect(screen.queryByRole('button', { name: 'Save changes' })).toBeNull();
  });

  it('opens each section from the rail, and says which section a pane is', async () => {
    const user = userEvent.setup();
    renderScreen();

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
    renderScreen();

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
  });

  it('is reached from the sidebar with Tab and Enter', async () => {
    const user = userEvent.setup();
    stubRecorderLinkRuntime({ link: 'connecting' }, null, {
      recorderSettings: () => CONFIGURED,
      audioDevices: () => MICROPHONES,
      recorderHotkeys: () => [],
    });
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

  it('heads each column of the account with the question the row answers', async () => {
    const user = userEvent.setup();
    renderScreen();
    await openSection(user, 'Storage');

    const headers = within(within(pane()).getByRole('table'))
      .getAllByRole('columnheader')
      .map((header) => header.textContent);

    expect(headers).toEqual(['Setting', 'How it is set today', 'What this window needs first']);
  });

  /*
   * The settings SPEC.md asks a settings screen for that this window still
   * cannot offer: what each is set by today, and the work that has to land.
   * Written out here rather than read from `SETTINGS_SECTIONS`, because a case
   * that walked the screen's own rows and checked their shape is satisfied by
   * rows somebody invented — which a review of the Games screen demonstrated by
   * replacing every row with `{ shows: 'x', needs: 'Issue #1' }` and watching
   * the suite stay green.
   */
  it.each([
    ['Recording', 'A bitrate in megabits', /preset rather than as a number/, /#181/],
    ['Recording', 'HDR', /no path for a setting/, /#99/],
    ['Recording', 'Recording format', /Matroska/, /#307/],
    ['Audio', 'Audio tracks, enable and level', /nothing mixes/, /#81/],
    ['Audio', 'A named playback device', /Windows is playing through/, /#316/],
    ['Storage', 'Where deleted recordings wait', /Library screen/, /#646/],
    ['Storage', 'How long the trash keeps something', /nothing expires/, /#646/],
    ['Per game', 'Record this game at all', /catalogue’s answer/, /#245/],
    ['Per game', 'Capture mode for this game', /one capture mode/, /#78/],
    [
      'Per game',
      'Where this game’s recordings go, and what they may occupy',
      /Storage section/,
      /section 31/,
    ],
    ['Notifications', 'Replay saved, bookmark added, screenshot taken', /nothing reports/, /#110/],
    ['Startup', 'Start Clipped’s window when I sign in', /the switch above/, /#308/],
  ])(
    'says, in %s, how "%s" is set today and what it is waiting for',
    async (section, label, today, needs) => {
      const user = userEvent.setup();
      renderScreen();
      await openSection(user, section);

      const [, todayCell, needsCell] = rowFor(label);
      expect(todayCell).toMatch(today);
      expect(needsCell).toMatch(needs);
    },
  );
});

/*
 * The notification switches (issue #252). They were four rows of an account
 * until this issue — "edit this key in this other file, by hand" — because they
 * lived in a second store this window kept for itself. They are settings now, so
 * what has to be true of them is what has to be true of any control: it draws
 * what the recorder said, and moving it sends something.
 *
 * The half these cases cannot reach is whether the switch then reaches the code
 * that decides about a toast. That is in Rust, in this window's own process, and
 * `notification_policy.rs` asserts it there.
 */
describe('the notification switches', () => {
  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
  });

  it('draws a switch rather than a list of the words the file spells it with', async () => {
    const user = userEvent.setup();
    renderScreen();
    await openSection(user, 'Notifications');

    const toggle = await screen.findByLabelText('A recording failed');

    expect(toggle).toBeChecked();
    expect(toggle).toHaveProperty('type', 'checkbox');
    // Every category, so that one the recorder sends and this screen never
    // lists is a failure here rather than a switch nobody can find.
    for (const label of [
      'A recording was interrupted',
      'The recorder cannot be reached',
      'A hotkey is unavailable',
    ]) {
      expect(await screen.findByLabelText(label)).toBeChecked();
    }
  });

  it('sends the switch somebody turned off to the recorder', async () => {
    const user = userEvent.setup();
    const runtime = renderScreen({
      applySettings: () => settingsWith('recording_failed', { value: 'false', overridden: true }),
    });

    await openSection(user, 'Notifications');
    await user.click(await screen.findByLabelText('A recording failed'));
    await user.click(screen.getByRole('button', { name: 'Save changes' }));

    await waitFor(() => {
      expect(saved(runtime)).toEqual([{ recording_failed: 'false' }]);
    });

    // And it redraws from what came back, rather than from what it sent.
    expect(await screen.findByLabelText('A recording failed')).not.toBeChecked();
    expect(screen.getByRole('button', { name: 'Reset A recording failed' })).toBeVisible();
  });

  it('draws a category the recorder says is off as off', async () => {
    const user = userEvent.setup();
    renderScreen({
      recorderSettings: () =>
        settingsWith('hotkey_unavailable', { value: 'false', overridden: true }),
    });

    await openSection(user, 'Notifications');

    expect(await screen.findByLabelText('A hotkey is unavailable')).not.toBeChecked();
    expect(screen.getByLabelText('A recording failed')).toBeChecked();
  });
});

/*
 * Starting at sign-in (issue #308). It is not a setting — no key in
 * `settings.json` carries it, and `apply_settings` does not reach it — so none
 * of the cases above touch it, and it fails in ways none of them would catch.
 *
 * The property that matters most is that the switch **writes the entry**. A
 * switch labelled "start the recorder when I sign in" that only moved on screen
 * is precisely the control AGENTS.md section 27 exists to forbid, and it is the
 * shape this screen carried until now: the row used to print the command and
 * ask somebody to go and run it in a terminal.
 */
describe('the start-at-login switch', () => {
  afterEach(() => {
    cleanup();
    vi.unstubAllGlobals();
  });

  it('is drawn where the recorder says the registry is, not where the window guesses', async () => {
    const user = userEvent.setup();
    renderScreen({ startAtLogin: () => AT_LOGIN });

    await openSection(user, 'Startup');
    const toggle = await screen.findByLabelText(/Start the recorder when I sign in/);

    expect(toggle).toBeChecked();
    // And the value itself, because there is nowhere else to see what will
    // actually run short of opening a registry editor.
    expect(pane()).toHaveTextContent('CurrentVersion');
    expect(pane()).toHaveTextContent('serve --watch-for-games');
  });

  it('asks the recorder to write the entry when it is turned on', async () => {
    const user = userEvent.setup();
    const runtime = renderScreen({
      startAtLogin: () => NOT_AT_LOGIN,
      setStartAtLogin: (args) => (args['enabled'] === true ? AT_LOGIN : NOT_AT_LOGIN),
    });

    await openSection(user, 'Startup');
    const toggle = await screen.findByLabelText(/Start the recorder when I sign in/);
    expect(toggle).not.toBeChecked();

    await user.click(toggle);

    // The whole of this issue: something arrived at the other end. Nothing in
    // this window can write a `Run` value, so if this array is empty the switch
    // is a light that turns itself on.
    await waitFor(() => {
      expect(switched(runtime)).toEqual([true]);
    });
    expect(await screen.findByLabelText(/Start the recorder when I sign in/)).toBeChecked();
  });

  it('asks the recorder to remove the entry when it is turned off', async () => {
    const user = userEvent.setup();
    const runtime = renderScreen({
      startAtLogin: () => AT_LOGIN,
      setStartAtLogin: () => NOT_AT_LOGIN,
    });

    await openSection(user, 'Startup');
    await user.click(await screen.findByLabelText(/Start the recorder when I sign in/));

    await waitFor(() => {
      expect(switched(runtime)).toEqual([false]);
    });
    expect(await screen.findByLabelText(/Start the recorder when I sign in/)).not.toBeChecked();
  });

  /*
   * The state that is neither on nor off: Windows will run the entry and the
   * program it names is gone, which is what a moved or reinstalled Clipped
   * leaves behind. Drawn as off, somebody turns it on and is told nothing
   * changed; drawn as plainly on, nothing starts at sign-in and nothing ever
   * says why.
   */
  it('says when the entry points at a Clipped that is no longer there', async () => {
    const user = userEvent.setup();
    renderScreen({
      startAtLogin: () => ({
        ...AT_LOGIN,
        missing_executable: String.raw`C:\Old\Clipped\clipped-recorder.exe`,
      }),
    });

    await openSection(user, 'Startup');
    const toggle = await screen.findByLabelText(/Start the recorder when I sign in/);

    expect(toggle, 'Windows will still try it, so the switch is on').toBeChecked();
    expect(pane()).toHaveTextContent('no longer at');
    expect(pane()).toHaveTextContent(String.raw`C:\Old\Clipped\clipped-recorder.exe`);
  });

  /*
   * And the switch stays where the registry is when a change is refused. A
   * switch that moved anyway would be telling somebody their machine is
   * arranged in a way it is not.
   */
  it('leaves the switch where it was when the recorder refuses to change it', async () => {
    const user = userEvent.setup();
    renderScreen({
      startAtLogin: () => NOT_AT_LOGIN,
      setStartAtLogin: () => {
        throw { code: 'internal', message: 'the registry refused while writing' };
      },
    });

    await openSection(user, 'Startup');
    await user.click(await screen.findByLabelText(/Start the recorder when I sign in/));

    await waitFor(() => {
      expect(pane()).toHaveTextContent('the registry refused while writing');
    });
    expect(await screen.findByLabelText(/Start the recorder when I sign in/)).not.toBeChecked();
  });

  /*
   * A recorder that cannot be asked is not a recorder that does not start at
   * sign-in, and only one of those two is fixed by pressing a switch
   * (AGENTS.md section 27). The default stub rejects, which is what this uses.
   */
  it('says it could not be read rather than drawing the switch off', async () => {
    const user = userEvent.setup();
    renderScreen();

    await openSection(user, 'Startup');

    await waitFor(() => {
      expect(pane()).toHaveTextContent('could not be read');
    });
    expect(screen.queryByLabelText(/Start the recorder when I sign in/)).toBeNull();
  });
});
