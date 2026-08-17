import type { SettingEntry, SettingsView } from '@clipped/shared';
import { describe, expect, it } from 'vitest';

import type { LibraryProblem } from './library';
import {
  describeLevel,
  firstRunOf,
  meterFraction,
  setupEntries,
  setupIsNeeded,
  SETUP_KEYS,
} from './setup';

/**
 * The decisions the first run makes without drawing anything (issue #109).
 *
 * Three of them, and each is a claim that would rot in silence if nothing held
 * it:
 *
 * - **whether setup has already happened.** It is read from the settings rather
 *   than from a flag of its own, so the reading has to be exactly right: a false
 *   negative walks somebody through setup every time they open the window, and a
 *   false positive means a fresh profile never sees it at all and SPEC.md
 *   section 45 step 3 never happens;
 * - **what the meter draws.** A linear bar barely moves for speech, which is the
 *   defect a level check exists to avoid;
 * - **what the sentence beside it says.** A muted microphone and an unplugged one
 *   both read as silence, and "say something louder" is the wrong answer to both.
 */

function entry(overrides: Partial<SettingEntry> & Pick<SettingEntry, 'key'>): SettingEntry {
  return {
    label: overrides.key,
    value: 'something',
    overridden: false,
    accepted: 'something',
    applies: true,
    ...overrides,
  };
}

/** A settings view carrying the two the first run asks about, plus one it does not. */
function view(overridden: Partial<Record<string, boolean>>): SettingsView {
  return {
    file: String.raw`C:\Users\alex\AppData\Local\Clipped\settings.json`,
    settings: [
      entry({ key: 'framerate', value: '60' }),
      entry({
        key: 'recording_directory',
        value: String.raw`C:\Users\alex\Videos\Clipped`,
        overridden: overridden['recording_directory'] ?? false,
      }),
      entry({
        key: 'microphone',
        value: 'default',
        overridden: overridden['microphone'] ?? false,
      }),
    ],
  };
}

const UNREACHABLE: LibraryProblem = {
  code: 'recorder_unreachable',
  message: 'the recorder is not running',
};

describe('whether this profile still has a first run to do', () => {
  it('is yes when nothing has configured either answer', () => {
    expect(setupIsNeeded(view({}))).toBe(true);
  });

  it('is no once both have been configured', () => {
    // What finishing setup produces, and therefore what stops it running again
    // on the next launch. If this were ever true for a configured profile the
    // window would open into the setup flow for ever.
    expect(setupIsNeeded(view({ recording_directory: true, microphone: true }))).toBe(false);
  });

  it('is yes when only one of the two has been configured', () => {
    // Somebody who set a microphone from the command line and never chose a
    // folder has not been through setup, and the half that is missing is the
    // half the flow is for. Both keys are checked separately rather than the
    // pair being treated as one.
    for (const key of SETUP_KEYS) {
      expect(setupIsNeeded(view({ [key]: true })), `${key} alone is not a finished setup`).toBe(
        true,
      );
    }
  });

  it('is no when the recorder does not send the settings at all', () => {
    // An older recorder is missing a setting rather than reporting an
    // unconfigured one. Counting an absent key as unconfigured would force
    // somebody into a step with no field in it and a Finish that could not
    // save.
    expect(setupIsNeeded({ file: 'settings.json', settings: [] })).toBe(false);
  });

  it('ignores the settings the flow does not ask about', () => {
    // `framerate` is unconfigured in every view above and must never be the
    // reason setup runs: the flow has no step for it, so it could not be
    // answered.
    const configured = view({ recording_directory: true, microphone: true });
    expect(configured.settings.some((row) => row.key === 'framerate' && !row.overridden)).toBe(
      true,
    );
    expect(setupIsNeeded(configured)).toBe(false);
  });
});

describe('the settings the flow puts in front of somebody', () => {
  it('are the two it asks about, in its own order rather than the recorder’s', () => {
    // The recorder sends the directory in the storage section and the
    // microphone in the audio one, in that order or another; the flow asks
    // where recordings go first because it is the answer somebody is most
    // likely to want to change.
    expect(setupEntries(view({})).map((row) => row.key)).toEqual([
      'recording_directory',
      'microphone',
    ]);
  });

  it('leave out a setting this recorder never sent', () => {
    const older: SettingsView = {
      file: 'settings.json',
      settings: [entry({ key: 'microphone', value: 'default' })],
    };
    expect(setupEntries(older).map((row) => row.key)).toEqual(['microphone']);
  });
});

describe('what the shell does with a settings read', () => {
  it('offers the flow only on a definite yes', () => {
    expect(firstRunOf({ state: 'read', value: view({}) })).toEqual({ state: 'needed' });
  });

  it('says nothing while the answer is still coming', () => {
    expect(firstRunOf({ state: 'reading' })).toEqual({ state: 'asking' });
  });

  it('does not treat an unreachable recorder as an unconfigured profile', () => {
    // The failure this prevents: a window that could not reach the recorder
    // walking somebody through a flow whose Finish was always going to fail,
    // every time the recorder happened to be starting up.
    expect(firstRunOf({ state: 'unread', problem: UNREACHABLE })).toEqual({
      state: 'unknown',
      problem: UNREACHABLE,
    });
  });
});

describe('where a peak sits on the meter', () => {
  it('is nothing for silence and the top for full scale', () => {
    expect(meterFraction(0)).toBe(0);
    expect(meterFraction(1)).toBe(1);
  });

  it('puts ordinary speech in the middle of the bar rather than at the bottom', () => {
    // The whole reason the scale is in decibels. A peak of 0.1 is a normal
    // speaking level; drawn linearly it is a tenth of the bar, which reads as
    // "this microphone is barely working". Anything that returned the
    // amplitude itself fails here.
    const speech = meterFraction(0.1);
    expect(speech).toBeGreaterThan(0.6);
    expect(speech).toBeLessThan(0.7);
  });

  it('draws everything below the floor as nothing rather than off the end', () => {
    // −60 dBFS is the floor. Below it the fraction would go negative, and a
    // bar with a negative value is one the browser draws as full or as empty
    // depending on which browser it is.
    expect(meterFraction(0.001)).toBe(0);
    expect(meterFraction(0.000001)).toBe(0);
  });

  it('never goes past the top for a sample above full scale', () => {
    expect(meterFraction(4)).toBe(1);
  });
});

describe('what the sentence beside the meter says', () => {
  it('says it is hearing you when the signal is above the floor', () => {
    expect(
      describeLevel({ state: 'heard', level: { device: 'Shure MV7', peak: 0.4, muted: false } }),
    ).toBe('Hearing you.');
  });

  it('asks for a sound when a working device is quiet', () => {
    expect(
      describeLevel({ state: 'heard', level: { device: 'Shure MV7', peak: 0, muted: false } }),
    ).toBe('Silent. Say something.');
  });

  it('blames the mute switch rather than the person, when the switch is on', () => {
    // A muted microphone reads as exactly the same silence as a quiet room.
    // Telling somebody to speak up is the vague message AGENTS.md section 28
    // is about, and this is the sentence that is checked first because of it.
    const sentence = describeLevel({
      state: 'heard',
      level: { device: 'Shure MV7', peak: 0, muted: true },
    });
    expect(sentence).toContain('Muted');
    expect(sentence).not.toContain('Say something');
  });

  it('says a device that is not there is not there', () => {
    // The other reason a meter never moves. `device` absent is the recorder
    // saying the endpoint was not open, which no reading of the peak can say.
    const sentence = describeLevel({ state: 'heard', level: { peak: 0 } });
    expect(sentence).toContain('Not connected');
    expect(sentence).not.toContain('Say something');
  });

  it('reports a recorder that cannot measure as a version skew, not as a broken microphone', () => {
    // `unknown_command` is a recorder older than this window. Sending somebody
    // to their sound settings over it would be the wrong instruction entirely.
    const sentence = describeLevel({
      state: 'unavailable',
      problem: { code: 'unknown_command', message: 'this recorder has no such command' },
    });
    expect(sentence).toContain('older than this window');
  });
});
