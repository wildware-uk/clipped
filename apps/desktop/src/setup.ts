import type { MicrophoneLevel, SettingEntry, SettingsView } from '@clipped/shared';
import { invoke } from '@tauri-apps/api/core';
import { useEffect, useState } from 'react';

import { asProblem, type LibraryProblem, type LibraryRead } from './library';
import { MICROPHONE, NO_DEVICE, readSettings, RECORDING_DIRECTORY } from './settings';

/**
 * The first run: what it asks, how it knows it has already happened, and how it
 * shows a microphone working (issue #109).
 *
 * # What this screen is, and is not
 *
 * It is **not** a second settings system. Every question it asks is a setting
 * the recorder already owns and the Settings screen already draws, asked through
 * the same `get_settings` / `apply_settings` and the same `settings.ts` hooks
 * (AGENTS.md section 55). What the first run adds is an order, a default in
 * every field, and one Save at the end — because SPEC.md section 45 asks that
 * somebody launch Clipped once, pick a microphone and a folder, close the
 * window, and never configure capture again.
 *
 * # Why there is no flag saying setup is done
 *
 * Because the settings are the flag. `SettingEntry.overridden` is the recorder's
 * own answer to "did this profile set this itself, or is it the value Clipped
 * ships with" (`Resolved::is_overridden`), and finishing setup writes both
 * answers explicitly — including when somebody accepted the detected defaults,
 * which is a choice they made rather than one they never got to. So
 * {@link setupIsNeeded} reads the state that already exists instead of adding a
 * second one that could disagree with it, and there is nothing to go stale if a
 * user deletes `settings.json`: setup runs again, which is right.
 *
 * # What the first run deliberately does not ask
 *
 * **The capture mode.** The recorder sends `capture_target` with `applies:
 * false` and the sentence "every recording captures the game's own window.
 * Reading this setting when a recording starts is issue #61". A setup step for
 * it would be the worst version of a control that does nothing, because it is
 * the first thing a user ever sees (AGENTS.md section 27). The last step says
 * what Clipped captures instead, which is true.
 *
 * **The replay hotkey.** No protocol command binds one — `get_hotkeys` reports
 * where each binding stands, and issue #54 is the screen that changes one — so
 * a combination picker here would save nothing. What the last step does instead
 * is *show* the replay key and whether Windows gave it to Clipped, which is
 * information somebody about to play a game needs and cannot get anywhere else
 * (SPEC.md section 45, step 7).
 */

/**
 * Where the first run lives.
 *
 * Not in `SCREENS`: it is not a destination in the sidebar, it is what the
 * window shows instead of the sidebar until it has been through once. The clip
 * playback route is outside `SCREENS` for the same kind of reason.
 */
export const SETUP_PATH = '/setup';

/**
 * The settings the first run asks about, in the order it asks them.
 *
 * Exactly SPEC.md section 45's step 3 — "select microphone and recording
 * directory" — and nothing else. Every other setting has a sensible shipped
 * default and a control on the Settings screen; adding them here would make the
 * first thing a user sees longer without making the walkthrough more likely to
 * work.
 */
export const SETUP_KEYS: readonly string[] = [RECORDING_DIRECTORY, MICROPHONE];

/**
 * Whether this profile still has a first run to do.
 *
 * True when the recorder sends one of {@link SETUP_KEYS} and says nothing has
 * configured it. A key the recorder does not send at all is **not** counted: an
 * older recorder is missing a setting rather than reporting an unconfigured one,
 * and forcing somebody through a setup step for a setting their recorder has
 * never heard of would be a step that could not be completed.
 */
export function setupIsNeeded(view: SettingsView): boolean {
  return SETUP_KEYS.some((key) => {
    const entry = view.settings.find((candidate) => candidate.key === key);
    return entry !== undefined && !entry.overridden;
  });
}

/** The settings the first run asks about, in its order, as the recorder sent them. */
export function setupEntries(view: SettingsView): readonly SettingEntry[] {
  return SETUP_KEYS.flatMap((key) => {
    const entry = view.settings.find((candidate) => candidate.key === key);
    return entry === undefined ? [] : [entry];
  });
}

/** Asks the recorder what one microphone is hearing right now. */
export async function readMicrophoneLevel(microphone: string): Promise<MicrophoneLevel> {
  return invoke<MicrophoneLevel>('microphone_level', { microphone });
}

/**
 * How long to wait between one reading and asking for the next.
 *
 * The recorder listens for a fixed slice inside each call, so this is the gap
 * between slices rather than the rate of the meter. Short enough that a meter
 * follows a voice, long enough that a settings screen is not opening an audio
 * endpoint as fast as Windows will let it.
 */
export const LEVEL_INTERVAL_MS = 100;

/** What the meter has to draw. */
export type LevelReading =
  /** Asked, nothing back yet. Only ever the first reading of a device. */
  | { readonly state: 'listening' }
  /** A reading. */
  | { readonly state: 'heard'; readonly level: MicrophoneLevel }
  /** The recorder could not listen, and said why. */
  | { readonly state: 'unavailable'; readonly problem: LibraryProblem };

/**
 * Above which peak a microphone counts as hearing something.
 *
 * About −34 dBFS. Chosen to sit above the noise floor of a microphone somebody
 * would want to record with and below ordinary speech, which peaks an order of
 * magnitude higher. It decides one sentence — "Hearing you" against "Silent" —
 * and nothing else: the meter itself draws whatever came back, so a signal below
 * this still moves the bar and somebody with a very quiet microphone can see it
 * working even while the sentence says silent.
 */
export const HEARD_ABOVE = 0.02;

/**
 * Where a peak sits on the meter, from `0` to `1`.
 *
 * Decibels rather than the linear amplitude, because a meter drawn linearly is
 * a meter that never moves: speech peaks around a tenth of full scale, which is
 * a tenth of the bar, while the difference between a microphone that is barely
 * working and one that is fine is most of the useful range and all of it below
 * that tenth. The floor is −60 dBFS, below which everything is drawn as nothing.
 */
export function meterFraction(peak: number): number {
  if (peak <= 0) {
    return 0;
  }
  const decibels = 20 * Math.log10(peak);
  return Math.min(1, Math.max(0, (decibels + 60) / 60));
}

/**
 * What the reading says, in one sentence.
 *
 * The order matters and is the whole point: a muted microphone and an unplugged
 * one both read as silence, and telling somebody to speak up when the answer is
 * a switch or a cable is the vague message AGENTS.md section 28 is about. So the
 * two reasons a meter would never move are checked before the meter is believed.
 */
export function describeLevel(reading: LevelReading): string {
  switch (reading.state) {
    case 'listening':
      return 'Listening…';
    case 'unavailable':
      return describeLevelProblem(reading.problem);
    case 'heard': {
      const { level } = reading;
      if (level.muted === true) {
        return 'Muted in Windows. Clipped would record silence until it is unmuted.';
      }
      if (level.device === undefined) {
        return 'Not connected. Clipped would record silence.';
      }
      return level.peak > HEARD_ABOVE ? 'Hearing you.' : 'Silent. Say something.';
    }
  }
}

/**
 * What the window says when it could not get a level at all.
 *
 * `unknown_command` is the one worth its own sentence, for the reason
 * `describeSettingsProblem` gives it one: a recorder built before this existed
 * has no `get_microphone_level`, and "your microphone could not be listened to"
 * would send somebody to Windows' sound settings over a version skew. The list
 * of devices still works either way, which is why this is a sentence beside the
 * chooser rather than a failure of the step.
 */
export function describeLevelProblem(problem: LibraryProblem): string {
  switch (problem.code) {
    case 'no_recorder_configured':
    case 'recorder_unreachable':
      return `Clipped could not reach the recorder, so there is no level to show. ${problem.message}`;
    case 'unknown_command':
    case 'not_implemented':
      return 'The recorder that is running is older than this window and cannot measure a microphone. Restarting Clipped starts the recorder that came with it.';
    default:
      return problem.message;
  }
}

/**
 * What `microphone` is hearing, asked again every {@link LEVEL_INTERVAL_MS}.
 *
 * `undefined` stops the polling, which is what a screen passes when no meter is
 * on show. So does {@link NO_DEVICE}: "record no microphone" is a setting
 * somebody chose and the recorder refuses to report a level for it, so asking
 * would produce a refusal to draw rather than the plain sentence a screen should
 * be showing.
 *
 * Each reading is asked for only after the last one came back, rather than on an
 * interval: a call that took longer than the gap would otherwise pile requests
 * up behind a device that is already struggling to open.
 */
export function useMicrophoneLevel(microphone: string | undefined): LevelReading {
  /*
   * The reading **and the device it was taken from**. Switching microphone has
   * to clear the meter — showing the last device's level beside the new
   * device's name would be the worst thing this screen could do — and holding
   * the pair is what makes that a thing this hook derives rather than a second
   * `setState` fired from the effect that starts the new polling.
   */
  const [seen, setSeen] = useState<{ readonly of: string; readonly reading: LevelReading }>();

  useEffect(() => {
    if (microphone === undefined || microphone === NO_DEVICE) {
      return undefined;
    }

    let current = true;
    let timer: ReturnType<typeof setTimeout> | undefined;

    const ask = (): void => {
      readMicrophoneLevel(microphone)
        .then((level) => {
          if (current) {
            setSeen({ of: microphone, reading: { state: 'heard', level } });
            timer = setTimeout(ask, LEVEL_INTERVAL_MS);
          }
        })
        .catch((thrown: unknown) => {
          // Stopped rather than retried. A refusal here is a version skew, a
          // recorder that is gone, or a device that cannot be opened — none of
          // which the next request would answer differently, and all of which
          // would otherwise be asked about several times a second for as long
          // as the screen is open.
          if (current) {
            setSeen({
              of: microphone,
              reading: { state: 'unavailable', problem: asProblem(thrown) },
            });
          }
        });
    };
    ask();

    return () => {
      current = false;
      if (timer !== undefined) {
        clearTimeout(timer);
      }
    };
  }, [microphone]);

  return seen !== undefined && seen.of === microphone ? seen.reading : { state: 'listening' };
}

/**
 * Whether this profile still has a first run to do, asked once.
 *
 * `unknown` is not "no". A recorder that could not be reached has not said that
 * this profile is configured, and a window that showed the setup flow over an
 * unreachable recorder would put somebody through steps whose Save was going to
 * fail. So the flow is offered only on a definite yes, and every other outcome
 * leaves the window where it was (AGENTS.md section 27).
 */
export type FirstRun =
  | { readonly state: 'asking' }
  | { readonly state: 'needed' }
  | { readonly state: 'done' }
  | { readonly state: 'unknown'; readonly problem: LibraryProblem };

/** Turns a settings read into the question the shell asks. */
export function firstRunOf(read: LibraryRead<SettingsView>): FirstRun {
  switch (read.state) {
    case 'reading':
      return { state: 'asking' };
    case 'unread':
      return { state: 'unknown', problem: read.problem };
    case 'read':
      return setupIsNeeded(read.value) ? { state: 'needed' } : { state: 'done' };
  }
}

/**
 * Whether this window should offer the first run, asked once when it opens.
 *
 * A read of its own rather than the settings screen's, because the shell needs
 * the answer before any screen has mounted and the settings screen is not one of
 * the screens that will have. One extra `get_settings` at start-up is the whole
 * cost of the gate.
 */
export function useFirstRun(): FirstRun {
  const [read, setRead] = useState<LibraryRead<SettingsView>>({ state: 'reading' });

  useEffect(() => {
    let current = true;
    readSettings()
      .then((settings) => {
        if (current) {
          setRead({ state: 'read', value: settings });
        }
      })
      .catch((thrown: unknown) => {
        if (current) {
          setRead({ state: 'unread', problem: asProblem(thrown) });
        }
      });
    return () => {
      current = false;
    };
  }, []);

  return firstRunOf(read);
}
