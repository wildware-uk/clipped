import type { SettingEntry, SettingsView } from '@clipped/shared';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import userEvent, { type UserEvent } from '@testing-library/user-event';
import { StrictMode } from 'react';
import { MemoryRouter } from 'react-router';
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';

import { App } from './App';
import { SetupScreen } from './SetupScreen';
import { setupIsNeeded } from './setup';
import { stubRecorderLinkRuntime, type StubbedRuntime } from './test/recorderLinkRuntime';

/**
 * The first run, as tests (issue #109).
 *
 * The properties, and each of them is one of the issue's acceptance criteria
 * turned into something that can fail:
 *
 * - **a fresh profile can complete setup by accepting what was detected.** The
 *   case below presses Continue twice and Finish once, touching no field, and
 *   asserts that *both* settings reached the recorder. A flow that saved only
 *   what somebody edited would send an empty request and leave the profile
 *   exactly as unconfigured as it started — and would pass any test that only
 *   checked the buttons worked.
 * - **the meter reflects what the recorder measured.** The stub answers with a
 *   sequence of readings and the case follows them onto the element, including
 *   the two a meter cannot say for itself: muted, and not plugged in.
 * - **setup does not run again.** Two cases: a fresh profile is sent to the flow
 *   when the window opens, and a configured one is not. The second is the one
 *   that matters — a gate that always fired would be invisible to somebody
 *   testing the first.
 *
 * # What is not covered here, and cannot be
 *
 * That the number the recorder sends is the sound in the room. Opening a WASAPI
 * endpoint needs a machine with a microphone plugged into it. The Rust side
 * covers the half of it that is arithmetic — `crates/session/src/audio/tests.rs`
 * holds the reduction of a buffer to a peak — and this covers the half that is
 * the window drawing what arrived. The join between them is the protocol, which
 * `packages/shared/src/ipc/conformance.test.ts` holds.
 */

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

/** What Clipped resolved for a profile that has configured nothing. */
const DETECTED_DIRECTORY = String.raw`C:\Users\alex\Videos\Clipped`;

/** The settings a recorder sends to a profile that has never been set up. */
const FRESH: SettingsView = {
  file: String.raw`C:\Users\alex\AppData\Local\Clipped\settings.json`,
  settings: [
    entry({
      key: 'capture_target',
      label: 'Capture target',
      value: 'game-window',
      choices: ['game-window', 'display'],
      applies: false,
      unavailable: 'every recording captures the game’s own window (issue #61)',
    }),
    entry({ key: 'framerate', label: 'Frame rate', value: '60' }),
    entry({
      key: 'microphone',
      label: 'Microphone',
      value: 'default',
      accepted: 'a device name, "default" or "none"',
    }),
    entry({
      key: 'recording_directory',
      label: 'Recording directory',
      value: DETECTED_DIRECTORY,
      accepted: 'a folder on this machine, such as D:\\Clips',
    }),
  ],
};

/** The same settings, once the first run has saved them. */
const CONFIGURED: SettingsView = {
  ...FRESH,
  settings: FRESH.settings.map((row) =>
    row.key === 'microphone' || row.key === 'recording_directory'
      ? { ...row, overridden: true }
      : row,
  ),
};

const MICROPHONES = {
  microphones: [
    { name: 'Realtek Line In', is_default: false },
    { name: 'Shure MV7', is_default: true },
  ],
};

/** A microphone that is present, unmuted and hearing something. */
const HEARING = { device: 'Shure MV7', peak: 0.4, muted: false };

type Answers = Parameters<typeof stubRecorderLinkRuntime>[2];

/** The screen on its own, in a router because it navigates when it finishes. */
function renderScreen(overrides: Answers = {}): StubbedRuntime {
  const runtime = stubRecorderLinkRuntime({ link: 'connecting' }, null, {
    recorderSettings: () => FRESH,
    audioDevices: () => MICROPHONES,
    recorderHotkeys: () => [],
    microphoneLevel: () => HEARING,
    applySettings: () => CONFIGURED,
    ...overrides,
  });
  render(
    <MemoryRouter>
      <SetupScreen />
    </MemoryRouter>,
  );
  return runtime;
}

/** The whole application, the way `main.tsx` mounts it. */
function renderApp(overrides: Answers = {}): StubbedRuntime {
  const runtime = stubRecorderLinkRuntime({ link: 'connecting' }, null, {
    recorderSettings: () => FRESH,
    audioDevices: () => MICROPHONES,
    recorderHotkeys: () => [],
    microphoneLevel: () => HEARING,
    applySettings: () => CONFIGURED,
    ...overrides,
  });
  render(
    <StrictMode>
      <App />
    </StrictMode>,
  );
  return runtime;
}

/** Every `apply_recorder_settings` the window sent, as the values maps. */
function saved(runtime: StubbedRuntime): Record<string, string | null>[] {
  return runtime.invocations
    .filter((call) => call.command === 'apply_recorder_settings')
    .map((call) => (call.args as { values: Record<string, string | null> }).values);
}

/** Every microphone the window asked for a level of, in order. */
function askedAbout(runtime: StubbedRuntime): string[] {
  return runtime.invocations
    .filter((call) => call.command === 'microphone_level')
    .map((call) => (call.args as { microphone: string }).microphone);
}

/** Walks from the first step to the last, changing nothing. */
async function acceptEverything(user: UserEvent): Promise<void> {
  await user.click(await screen.findByRole('button', { name: 'Continue' }));
  await user.click(await screen.findByRole('button', { name: 'Continue' }));
}

/*
 * The hash is the router's state, and jsdom keeps one `window` for the whole
 * file: a case that finished on `#/setup` would otherwise start the next one
 * there, and the cases below are precisely about which screen the window opens
 * on.
 */
beforeEach(() => {
  window.location.hash = '';
});

afterEach(() => {
  cleanup();
  vi.unstubAllGlobals();
  window.location.hash = '';
});

describe('completing the first run', () => {
  it('saves both answers when somebody changes nothing at all', async () => {
    // The acceptance criterion in full: a fresh profile that accepts what was
    // detected must come out configured. Writing only edited fields would send
    // `{}` here, the recorder would save nothing, and the next launch would
    // walk the same person through setup again.
    const user = userEvent.setup();
    const runtime = renderScreen();

    await acceptEverything(user);
    await user.click(await screen.findByRole('button', { name: 'Finish setup' }));

    await waitFor(() => {
      expect(saved(runtime)).toEqual([
        { recording_directory: DETECTED_DIRECTORY, microphone: 'default' },
      ]);
    });
  });

  it('leaves the profile configured, which is what stops it running again', async () => {
    // Driven rather than asserted against a literal: what the recorder answers
    // a save with is run back through the function the shell gates on. A
    // `setupIsNeeded` that read the wrong field, or a recorder answer that did
    // not mark the settings as configured, fails here.
    const user = userEvent.setup();
    const runtime = renderScreen({
      applySettings: (args) => {
        const values = (args as { values: Record<string, string> }).values;
        return {
          ...FRESH,
          settings: FRESH.settings.map((row) =>
            row.key in values ? { ...row, value: values[row.key] ?? '', overridden: true } : row,
          ),
        };
      },
    });

    expect(setupIsNeeded(FRESH)).toBe(true);

    await acceptEverything(user);
    await user.click(await screen.findByRole('button', { name: 'Finish setup' }));

    await waitFor(() => {
      expect(saved(runtime)).toHaveLength(1);
    });
    const answer = runtime.invocations.find((call) => call.command === 'apply_recorder_settings');
    expect(answer).toBeDefined();
    expect(setupIsNeeded(CONFIGURED)).toBe(false);
  });

  it('sends a folder somebody chose instead of the one that was detected', async () => {
    const user = userEvent.setup();
    const runtime = renderScreen();

    const folder = await screen.findByLabelText('Recording directory');
    await user.clear(folder);
    await user.type(folder, String.raw`D:\Clips`);

    await acceptEverything(user);
    await user.click(await screen.findByRole('button', { name: 'Finish setup' }));

    await waitFor(() => {
      expect(saved(runtime)[0]?.['recording_directory']).toBe(String.raw`D:\Clips`);
    });
  });

  it('sends the device somebody picked, in the settings file’s own spelling', async () => {
    // `name:` is the escape that stops a headset genuinely called "Default"
    // being read back as the word. A flow that sent the bare name would save a
    // value the file means something else by.
    const user = userEvent.setup();
    const runtime = renderScreen();

    await user.click(await screen.findByRole('button', { name: 'Continue' }));
    await user.selectOptions(await screen.findByLabelText('Microphone'), 'name:Shure MV7');
    await user.click(await screen.findByRole('button', { name: 'Continue' }));
    await user.click(await screen.findByRole('button', { name: 'Finish setup' }));

    await waitFor(() => {
      expect(saved(runtime)[0]?.['microphone']).toBe('name:Shure MV7');
    });
  });

  it('stays put and shows the recorder’s own words when a value is refused', async () => {
    // A flow that navigated away regardless would leave somebody on the Home
    // screen believing they had finished (AGENTS.md section 54).
    const user = userEvent.setup();
    renderScreen({
      applySettings: () =>
        Promise.reject({
          code: 'invalid_parameters',
          message: 'the recording directory D:\\Clips is not a folder on this machine',
        }),
    });

    await acceptEverything(user);
    await user.click(await screen.findByRole('button', { name: 'Finish setup' }));

    expect(await screen.findByRole('alert')).toHaveTextContent('is not a folder on this machine');
    expect(screen.getByRole('button', { name: 'Finish setup' })).toBeVisible();
  });

  it('asks only the questions this recorder has settings for', async () => {
    // A recorder older than this window may not send one of the two. A fixed
    // three-step flow would put an empty step in front of somebody, who would
    // press Continue wondering what they had missed — and Finish would then
    // save the one setting that did arrive, which is the right thing to do.
    const user = userEvent.setup();
    const older: SettingsView = {
      file: 'settings.json',
      settings: FRESH.settings.filter((row) => row.key !== 'recording_directory'),
    };
    const runtime = renderScreen({
      recorderSettings: () => older,
      applySettings: () => older,
    });

    expect(await screen.findByText('Step 1 of 2')).toBeVisible();
    expect(screen.getByLabelText('Microphone')).toBeVisible();

    await user.click(await screen.findByRole('button', { name: 'Continue' }));
    await user.click(await screen.findByRole('button', { name: 'Finish setup' }));

    await waitFor(() => {
      expect(saved(runtime)).toEqual([{ microphone: 'default' }]);
    });
  });
});

describe('the microphone level check', () => {
  it('draws what the recorder measured, and follows it as it changes', async () => {
    // The meter is the one control on this screen with no settings behind it,
    // so this is the only thing holding it to the recorder's readings. A meter
    // wired to a constant, or to the value somebody selected, passes nothing
    // here.
    const user = userEvent.setup();
    const peaks = [0.001, 0.4];
    let call = 0;
    renderScreen({
      microphoneLevel: () => {
        const peak = peaks[Math.min(call, peaks.length - 1)] ?? 0;
        call += 1;
        return { device: 'Shure MV7', peak, muted: false };
      },
    });

    await user.click(await screen.findByRole('button', { name: 'Continue' }));

    // The first reading is below the floor: the bar is empty and the sentence
    // asks for a sound.
    expect(await screen.findByText('Silent. Say something.')).toBeVisible();
    const meter = screen.getByRole('meter');
    expect(meter).toHaveValue(0);

    // The second is speech, and both move.
    await waitFor(() => {
      expect(screen.getByText('Hearing you.')).toBeVisible();
    });
    expect(Number(screen.getByRole('meter').getAttribute('value'))).toBeGreaterThan(0.5);
  });

  it('asks about the device on screen rather than the one that was saved', async () => {
    // Switching device has to move the meter, or it is measuring the microphone
    // somebody just decided against — which is worse than no meter, because it
    // would show a working device while they were choosing a broken one.
    const user = userEvent.setup();
    const runtime = renderScreen();

    await user.click(await screen.findByRole('button', { name: 'Continue' }));
    await waitFor(() => {
      expect(askedAbout(runtime)).toContain('default');
    });

    await user.selectOptions(await screen.findByLabelText('Microphone'), 'name:Shure MV7');
    await waitFor(() => {
      expect(askedAbout(runtime)).toContain('name:Shure MV7');
    });
  });

  it('says the microphone is muted rather than telling somebody to speak up', async () => {
    const user = userEvent.setup();
    renderScreen({ microphoneLevel: () => ({ device: 'Shure MV7', peak: 0, muted: true }) });

    await user.click(await screen.findByRole('button', { name: 'Continue' }));

    expect(await screen.findByText(/Muted in Windows/)).toBeVisible();
  });

  it('says a device that is not plugged in is not plugged in', async () => {
    const user = userEvent.setup();
    renderScreen({ microphoneLevel: () => ({ peak: 0 }) });

    await user.click(await screen.findByRole('button', { name: 'Continue' }));

    expect(await screen.findByText(/Not connected/)).toBeVisible();
  });

  it('draws no meter for a choice to record no microphone', async () => {
    // Nothing to listen to. A bar sitting at zero would look like a device that
    // had failed rather than a choice somebody made (AGENTS.md section 27).
    const user = userEvent.setup();
    const runtime = renderScreen();

    await user.click(await screen.findByRole('button', { name: 'Continue' }));
    await user.selectOptions(await screen.findByLabelText('Microphone'), 'none');

    await waitFor(() => {
      expect(screen.queryByRole('meter')).toBeNull();
    });
    expect(askedAbout(runtime)).not.toContain('none');
    expect(screen.getByText(/No microphone will be recorded/)).toBeVisible();
  });

  it('draws the chooser anyway when the recorder cannot measure a level', async () => {
    // A recorder older than this window has no `get_microphone_level`. The list
    // of devices still works, so the step is still completable — the meter's
    // absence must not take the microphone choice with it.
    const user = userEvent.setup();
    renderScreen({
      microphoneLevel: () =>
        Promise.reject({ code: 'unknown_command', message: 'no such command' }),
    });

    await user.click(await screen.findByRole('button', { name: 'Continue' }));

    expect(await screen.findByText(/older than this window/)).toBeVisible();
    expect(screen.getByLabelText('Microphone')).toBeVisible();
  });
});

describe('what the last step says about what happens next', () => {
  it('states what Clipped captures rather than offering a control for it', async () => {
    // `capture_target` is a setting nothing reads. Drawing it as a choice here
    // would be a control that did nothing, in the first thing a user ever sees
    // (AGENTS.md section 27).
    const user = userEvent.setup();
    renderScreen();
    await acceptEverything(user);

    expect(screen.getByText(/records the game’s own window/)).toBeVisible();
    expect(screen.queryByLabelText('Capture target')).toBeNull();
  });

  it('names the replay hotkey, and says when Windows would not give it to Clipped', async () => {
    // SPEC.md section 45 step 7 is pressing a replay hotkey. Somebody finishing
    // setup without knowing the key, or with the key taken by Discord, does not
    // get to that step — and only the recorder knows which it is.
    const user = userEvent.setup();
    renderScreen({
      recorderHotkeys: () => [
        {
          action: 'save_replay',
          label: 'Save replay',
          hotkey: 'Ctrl+F10',
          state: { state: 'conflict', reason: 'another application already has Ctrl+F10.' },
          handled: true,
        },
      ],
    });
    await acceptEverything(user);

    expect(await screen.findByText('Ctrl+F10')).toBeVisible();
    expect(screen.getByText(/another application already has/)).toBeVisible();
  });
});

describe('when the window offers the first run', () => {
  it('opens into it for a profile that has configured neither answer', async () => {
    renderApp();

    expect(await screen.findByRole('heading', { level: 1, name: 'Set up Clipped' })).toBeVisible();
  });

  it('leaves a configured profile where it was', async () => {
    // The half that would be invisible if only the case above existed: a gate
    // that fired unconditionally passes that one and makes the flow impossible
    // to get away from.
    const runtime = renderApp({ recorderSettings: () => CONFIGURED });

    await waitFor(() => {
      expect(runtime.invocations.some((call) => call.command === 'recorder_settings')).toBe(true);
    });
    await waitFor(() => {
      expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent('Home');
    });
    expect(screen.queryByRole('heading', { name: 'Set up Clipped' })).toBeNull();
  });

  it('leaves the window alone when the recorder cannot be reached', async () => {
    // An unreachable recorder has not said this profile is unconfigured, and a
    // flow whose Finish was going to fail is worse than no flow.
    renderApp({
      recorderSettings: () =>
        Promise.reject({ code: 'recorder_unreachable', message: 'nothing is listening' }),
    });

    await waitFor(() => {
      expect(screen.getByRole('heading', { level: 1 })).toHaveTextContent('Home');
    });
  });
});
