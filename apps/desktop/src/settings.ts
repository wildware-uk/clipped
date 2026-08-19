import type { AudioDevices, SettingEntry, SettingsView, StartAtLogin } from '@clipped/shared';
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import { useCallback, useEffect, useState } from 'react';

import { asProblem, type LibraryProblem, type LibraryRead } from './library';
import { MAXIMUM_AGE_DAYS, MAXIMUM_USAGE, MINIMUM_FREE_SPACE } from './storage';

/**
 * The Settings screen: what it can change, and what it can only account for
 * (issue #51).
 *
 * # Where a setting lives, and how this window reaches it
 *
 * In `%LOCALAPPDATA%\Clipped\settings.json`, which `clipped_session::config`
 * owns — its defaults, its types, its validation, its per-game layering and its
 * migrations (`docs/configuration.md`). This window may link one crate of the
 * repository's workspace, `clipped-ipc`, and
 * `tests/integration/tests/workspace_layering.rs` enforces it, so it neither
 * reads that file nor validates a value itself: **it asks the recorder**
 * (`get_settings`, `apply_settings`, `get_audio_devices`). A window that read
 * the file would be a second implementation of its versioning and validation,
 * against the file somebody's settings live in (AGENTS.md section 55).
 *
 * Everything a control needs comes back with the setting: its label, what it
 * resolves to, whether it was configured, the values it accepts, and — the one
 * that decides whether a control is drawn at all — whether anything reads it
 * when a recording starts. A setting the file can carry and no recording acts
 * on is drawn as its value and the sentence saying what it is waiting for,
 * never as a working control (AGENTS.md section 27).
 *
 * # What is still an account rather than a control
 *
 * The rows in {@link SETTINGS_SECTIONS}. Each one is a setting SPEC.md asks a
 * settings screen for that has nowhere to be saved, or nothing behind it, and
 * each names the issue that would build it. They are held against the Rust that
 * owns them by `settingsConformance.test.ts`.
 */

/**
 * Where Clipped keeps a value, when anything keeps it.
 *
 * One file. There were two until issue #252 — the notification switches lived
 * in a `notifications.json` of this window's own, with a version field and a
 * reader of their own — and a union of one is what says that is over.
 */
export type SettingsFile = 'settings.json';

/** The key a setting has in the file that carries it. */
export interface SettingLocation {
  /** The key, spelled as the file spells it. */
  readonly name: string;
  /** Which file that key is in. */
  readonly file: SettingsFile;
}

/** One setting this window cannot offer, and where it stands. */
export interface SettingRow {
  /** The setting's name in the words a person reads. */
  readonly label: string;
  /**
   * The key that carries it, where a file carries one.
   *
   * Absent means no store has a key for this setting at all, which is a
   * different thing from a key nothing reads — and the reason it is worth
   * saying: SPEC.md asks for the setting, and there is nowhere to put it yet.
   */
  readonly key?: SettingLocation;
  /** How the setting is set today, in one sentence. */
  readonly today: string;
  /** The command that sets it today, where a command does. */
  readonly run?: string;
  /** What has to land before this window can hold the control, ending in the
   * issue that lands it. */
  readonly needs: string;
}

/** One section of the screen: what the rail opens. */
export interface SettingsSection {
  /** Stable identifier, used in the rail and in the element ids. */
  readonly id: string;
  /** The rail's label for it. */
  readonly label: string;
  /** What the section is about. */
  readonly lead: string;
  /**
   * The settings the recorder sends that this section draws controls for, by
   * the key each has in the settings file, in the order they are drawn.
   *
   * A key the recorder does not send is not drawn — an older recorder is
   * missing settings rather than showing empty controls — and a key the
   * recorder sends that no section lists would be a setting this screen never
   * offered, which `settingsConformance.test.ts` fails on.
   */
  readonly keys: readonly string[];
  /** The settings this window cannot offer, and what each is waiting for. */
  readonly rows: readonly SettingRow[];
}

/** Clipped's settings file, as a user would find it. */
export const SETTINGS_FILE = String.raw`%LOCALAPPDATA%\Clipped\settings.json`;

/**
 * The one section with something live in it beyond the settings themselves.
 *
 * Named here rather than spelled in the screen, so that renaming the section
 * cannot silently stop the hotkey list being drawn.
 */
export const HOTKEYS_SECTION = 'hotkeys';

/**
 * The section with the other thing that is live and is not a setting.
 *
 * Named here for the reason {@link HOTKEYS_SECTION} is: renaming the section
 * must not silently stop the start-at-login switch being drawn.
 */
export const STARTUP_SECTION = 'startup';

/**
 * The section holding the notification switches.
 *
 * Named here for the reason {@link HOTKEYS_SECTION} is, and for one more: these
 * four are the settings this window itself acts on, so a test about whether a
 * switch reaches anything has to be able to find them.
 */
export const NOTIFICATIONS_SECTION = 'notifications';

/**
 * The keys the notification switches have in the settings file.
 *
 * The same words `clipped_session::config::notifications` writes and the same
 * words the desktop host matches on when it decides whether to show a toast, so
 * a rename here would draw a control over a switch nothing reads.
 * `settingsConformance.test.ts` holds all three lists equal.
 */
export const NOTIFICATION_KEYS = [
  'recording_failed',
  'recording_interrupted',
  'recorder_unavailable',
  'hotkey_unavailable',
] as const;

/**
 * The keys the hotkey bindings have on the settings commands.
 *
 * `hotkey_` and the action's own name, which is how the recorder sends them
 * (`apps/recorder/src/settings.rs`): the settings *file* keeps them under a
 * `hotkeys` object, and these commands are one flat namespace shared with the
 * per-game settings, so the prefix is what stops `save_replay` the binding
 * colliding with `save_replay` the protocol command.
 *
 * The order is the one `clipped_hotkeys::ACTIONS` lists, which is the order a
 * screen should draw them in. `settingsConformance.test.ts` reads the actions
 * out of the Rust and fails if this list drifts from them.
 */
export const HOTKEY_KEYS = [
  'hotkey_save_replay',
  'hotkey_add_bookmark',
  'hotkey_take_screenshot',
  'hotkey_toggle_recording',
  'hotkey_mute_microphone',
  'hotkey_toggle_microphone',
  'hotkey_open_overlay',
] as const;

/** The key the recording directory has in the settings file. */
export const RECORDING_DIRECTORY = 'recording_directory';

/**
 * The section holding the storage limits and the measurement they act on.
 *
 * Named here for the reason {@link HOTKEYS_SECTION} is, and for a sharper one:
 * this is the one section whose Save can delete somebody's recordings, so the
 * screen has to be able to find it in order to confirm against what a limit
 * would take before it takes it (SPEC.md section 27, issue #95).
 */
export const STORAGE_SECTION = 'storage';

/**
 * Whether a setting is a switch rather than a field or a list.
 *
 * True exactly when the only values the recorder will accept are `true` and
 * `false`, which is what a boolean setting is. Asked of the answer rather than
 * from a list of keys kept here, so that a switch a newer recorder adds is drawn
 * as one without this window being taught its name (AGENTS.md section 55).
 */
export function isSwitch(entry: SettingEntry): boolean {
  const choices = entry.choices ?? [];
  return choices.length === 2 && choices.includes('true') && choices.includes('false');
}

/** The key the microphone has, which is the one control with a device list. */
export const MICROPHONE = 'microphone';

/** The two values an audio setting takes that are not a device name. */
export const DEFAULT_DEVICE = 'default';

/** Record nothing from this source. */
export const NO_DEVICE = 'none';

/**
 * What a device name is prefixed with in the settings file.
 *
 * The escape that lets somebody whose headset is genuinely called "Default"
 * name it (`clipped_session::config::document`). The window writes it so that a
 * device is never read back as one of the two words above.
 */
export const DEVICE_PREFIX = 'name:';

/** Asks the recorder for every setting, as it now stands. */
export async function readSettings(): Promise<SettingsView> {
  return invoke<SettingsView>('recorder_settings');
}

/**
 * Changes settings, and answers with the settings as the recorder now holds
 * them.
 *
 * `null` clears one, which is Reset: it returns the setting to the value
 * Clipped ships with and keeps following it, which writing today's default in
 * as a value would not.
 */
export async function saveSettings(
  values: Readonly<Record<string, string | null>>,
): Promise<SettingsView> {
  return invoke<SettingsView>('apply_recorder_settings', { values });
}

/** Asks the recorder which microphones this machine has. */
export async function readAudioDevices(): Promise<AudioDevices> {
  return invoke<AudioDevices>('audio_devices');
}

/** Asks the recorder whether it starts when this user signs in. */
export async function readStartAtLogin(): Promise<StartAtLogin> {
  return invoke<StartAtLogin>('start_at_login');
}

/**
 * Turns starting at login on or off, and answers with where it now stands.
 *
 * The recorder writes the value, because the value names the recorder's own
 * executable and this window would have to guess at it (issue #308).
 */
export async function saveStartAtLogin(enabled: boolean): Promise<StartAtLogin> {
  return invoke<StartAtLogin>('set_start_at_login', { enabled });
}

/**
 * Asks the operating system for a folder to record into.
 *
 * `null` when the dialog was dismissed, which is not a failure and must not be
 * reported as one.
 */
export async function chooseRecordingDirectory(current: string): Promise<string | null> {
  const chosen = await open({
    directory: true,
    multiple: false,
    title: 'Where should Clipped keep recordings?',
    ...(current === '' ? {} : { defaultPath: current }),
  });
  return typeof chosen === 'string' ? chosen : null;
}

/**
 * What the window says about settings it could not read or save.
 *
 * `unknown_command` is the one worth its own sentence: a recorder built before
 * issue #51 has no `get_settings` at all and refuses `apply_settings` as not
 * implemented, so "your settings could not be read" would send somebody looking
 * at their disk rather than restarting Clipped (AGENTS.md section 45).
 */
export function describeSettingsProblem(problem: LibraryProblem): string {
  switch (problem.code) {
    case 'no_recorder_configured':
    case 'recorder_unreachable':
      return `Clipped could not ask the recorder for your settings, so nothing here can be changed. ${problem.message}`;
    case 'unknown_command':
    case 'not_implemented':
      return 'The recorder that is running is older than this window and cannot read or change settings. Restarting Clipped starts the recorder that came with it.';
    default:
      return problem.message;
  }
}

/** One thing a microphone control offers. */
export interface MicrophoneOption {
  /** The value the settings file would carry, which is what a control sends. */
  readonly value: string;
  /** What it is called in the list. */
  readonly label: string;
}

/**
 * What a microphone setting offers: the two words, then this machine's devices.
 *
 * Here rather than in a screen because two screens draw the chooser — the
 * Settings screen and the first run (issue #109) — and a second copy of this
 * list would be a second answer to "what can I record from", differing in
 * whichever of them somebody forgot to change (AGENTS.md section 55).
 */
export function microphoneOptions(
  entry: SettingEntry,
  devices: LibraryRead<AudioDevices>,
): readonly MicrophoneOption[] {
  const listed = devices.state === 'read' ? devices.value.microphones : [];
  const options = [
    { value: DEFAULT_DEVICE, label: 'Whichever Windows is using' },
    { value: NO_DEVICE, label: 'Do not record a microphone' },
    ...listed.map((device) => ({
      value: `${DEVICE_PREFIX}${device.name}`,
      label: device.is_default ? `${device.name} (default)` : device.name,
    })),
  ];

  // A configured device that is not in the list is one that is unplugged, and
  // it stays on offer: dropping it would silently change what is recorded to
  // whatever the list happened to start with (AGENTS.md section 27).
  const named = deviceNamed(entry.value);
  if (named !== undefined && !options.some((option) => option.value === entry.value)) {
    options.push({ value: entry.value, label: `${named} (not connected)` });
  }
  return options;
}

/** The device a microphone setting names, without the prefix that made it literal. */
export function deviceNamed(value: string): string | undefined {
  if (value === DEFAULT_DEVICE || value === NO_DEVICE) {
    return undefined;
  }
  return value.startsWith(DEVICE_PREFIX) ? value.slice(DEVICE_PREFIX.length) : value;
}

/** What a save is doing, or what it did. */
export type SaveState =
  | { readonly state: 'idle' }
  | { readonly state: 'saving' }
  | { readonly state: 'saved' }
  | { readonly state: 'refused'; readonly problem: LibraryProblem };

/** The settings, and the one way this screen changes them. */
export interface SettingsControl {
  /** The settings as the recorder last reported them. */
  readonly read: LibraryRead<SettingsView>;
  /** What the last save did. */
  readonly save: SaveState;
  /**
   * Sends changes, and redraws from what came back.
   *
   * Answers whether the recorder took them, which is what a screen clears its
   * edits on: a refused value has to stay on screen to be corrected.
   */
  readonly apply: (values: Readonly<Record<string, string | null>>) => Promise<boolean>;
}

/**
 * The settings, read once when the screen is opened and re-read from every
 * change the recorder accepts.
 *
 * Nothing is drawn from what was sent: the answer to `apply_settings` is the
 * settings as the recorder now holds them, so a value it refused, or one
 * another window changed a moment earlier, cannot be drawn as saved
 * (`crates/ipc/src/settings.rs`).
 */
export function useSettings(): SettingsControl {
  const [read, setRead] = useState<LibraryRead<SettingsView>>({ state: 'reading' });
  const [save, setSave] = useState<SaveState>({ state: 'idle' });

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

  const apply = useCallback(async (values: Readonly<Record<string, string | null>>) => {
    setSave({ state: 'saving' });
    try {
      const settings = await saveSettings(values);
      setRead({ state: 'read', value: settings });
      setSave({ state: 'saved' });
      return true;
    } catch (thrown: unknown) {
      // The edits stay on screen: a refused value is one somebody has to be
      // able to correct, and a form that cleared itself would take the rest of
      // their work with it (AGENTS.md section 45).
      setSave({ state: 'refused', problem: asProblem(thrown) });
      return false;
    }
  }, []);

  return { read, save, apply };
}

/**
 * The microphones this machine has, asked for when the screen opens and again
 * whenever the window comes back to the front.
 *
 * Asked again rather than kept, because the answer goes stale in the one way
 * that matters: somebody plugs in the headset they are about to choose. The
 * recorder enumerates the endpoints on every request precisely so that it can
 * be asked twice (`apps/recorder/src/serve.rs`), and a window that asked once
 * would offer a list of the devices that happened to be there when the screen
 * opened (issue #308).
 *
 * Coming back to the front is the trigger because it is when it is worth
 * asking and no more often: plugging a device in means going to the machine and
 * returning to the window. Polling would ask hundreds of times for one answer
 * that changes twice a day.
 *
 * The previous list stays on screen while the new one is fetched. Dropping back
 * to `reading` would blank a control somebody may be in the middle of using,
 * for an answer that is almost always identical.
 */
export function useAudioDevices(): LibraryRead<AudioDevices> {
  const [read, setRead] = useState<LibraryRead<AudioDevices>>({ state: 'reading' });

  useEffect(() => {
    let current = true;
    const ask = () => {
      readAudioDevices()
        .then((devices) => {
          if (current) {
            setRead({ state: 'read', value: devices });
          }
        })
        .catch((thrown: unknown) => {
          if (current) {
            setRead({ state: 'unread', problem: asProblem(thrown) });
          }
        });
    };

    ask();
    window.addEventListener('focus', ask);
    return () => {
      current = false;
      window.removeEventListener('focus', ask);
    };
  }, []);

  return read;
}

/** Whether the recorder starts at sign-in, and the one way this screen changes it. */
export interface StartAtLoginControl {
  /** The arrangement as the recorder last reported it. */
  readonly read: LibraryRead<StartAtLogin>;
  /** What the last change did. */
  readonly save: SaveState;
  /**
   * Moves the switch, and redraws from what came back.
   *
   * Answers whether the recorder took it, so that a refused change leaves the
   * switch where the registry actually is rather than where it was pushed.
   */
  readonly set: (enabled: boolean) => Promise<boolean>;
}

/**
 * Whether the recorder starts at sign-in, read when the screen opens.
 *
 * Not part of {@link useSettings}: it is not in `settings.json` and
 * `apply_settings` does not reach it. It is a `Run` value under this account
 * that Windows reads once, at sign-in, and that Windows also lists in
 * Settings > Apps > Startup with a switch of its own — so it is read back from
 * the registry after every change rather than assumed (issue #308).
 */
export function useStartAtLogin(): StartAtLoginControl {
  const [read, setRead] = useState<LibraryRead<StartAtLogin>>({ state: 'reading' });
  const [save, setSave] = useState<SaveState>({ state: 'idle' });

  useEffect(() => {
    let current = true;
    readStartAtLogin()
      .then((state) => {
        if (current) {
          setRead({ state: 'read', value: state });
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

  const set = useCallback(async (enabled: boolean) => {
    setSave({ state: 'saving' });
    try {
      setRead({ state: 'read', value: await saveStartAtLogin(enabled) });
      setSave({ state: 'saved' });
      return true;
    } catch (thrown: unknown) {
      // The switch is left reading whatever the last successful answer said,
      // which is where the registry is. A switch that moved on a refused write
      // would be telling somebody their machine is arranged in a way it is not
      // (AGENTS.md section 27).
      setSave({ state: 'refused', problem: asProblem(thrown) });
      return false;
    }
  }, []);

  return { read, save, set };
}

/**
 * Every section of the screen: the settings it draws, and what it can only
 * account for.
 *
 * The order is the deck's: what a recording is made at, then its audio, then
 * where the files go, then the keys, then what interrupts you, then what
 * happens at sign-in.
 */
export const SETTINGS_SECTIONS: readonly SettingsSection[] = [
  {
    id: 'recording',
    label: 'Recording',
    lead:
      'What a recording is made at. A change here reaches the next recording; nothing re-reads ' +
      'these while one is running.',
    keys: [
      'capture_target',
      'resolution',
      'framerate',
      'codec',
      'encoder',
      'replay_window_seconds',
    ],
    rows: [
      {
        label: 'Quality preset and bitrate',
        today:
          'Nothing. A recording’s bitrate is derived rather than chosen, and no key carries a ' +
          'preset or a bitrate.',
        needs:
          'Issue #181 to make the bitrate a choice, and issue #62 for the Performance, ' +
          'Balanced, High and Ultra presets SPEC.md section 10 draws.',
      },
      {
        label: 'Recording format',
        today:
          'Matroska. Copying a finished recording into MP4 without re-encoding is built and ' +
          'nothing calls it automatically, so there is no format to choose between yet.',
        needs:
          'Issue #307: a container setting, and the session work that acts on it (SPEC.md ' +
          'section 15).',
      },
    ],
  },
  {
    id: 'audio',
    label: 'Audio',
    lead:
      'Which endpoints a recording captures. Each source becomes a track of its own in the file, ' +
      'so they can be edited apart (SPEC.md section 12).',
    keys: [MICROPHONE, 'system_audio'],
    rows: [
      {
        label: 'Audio tracks, enable and level',
        today:
          'Nothing. A recording carries the tracks its sources produced and nothing mixes, ' +
          'mutes or levels them here.',
        needs:
          'Issue #81 for the track list SPEC.md section 12 draws, and issue #33 for routing an ' +
          'application to a track.',
      },
      {
        label: 'A named playback device',
        today:
          'Not offered. System audio is recorded from the endpoint Windows is playing through, ' +
          'and this build cannot record a different one — so the choice above is between that ' +
          'and nothing.',
        needs: 'Issue #316, which is what would let an endpoint be named.',
      },
    ],
  },
  {
    id: 'storage',
    label: 'Storage',
    /*
     * "the next session" and not "the next recording", because those differ for
     * this one setting. A recording asked for from this window honours a new
     * folder at once; an automatic one moves between sittings and never during
     * one, so that a sitting's session record is never separated from the files
     * it names (AGENTS.md section 56, issue #609). The recorder says so on the
     * row itself while a sitting is holding a change back.
     */
    lead:
      'Where recordings go, what the library occupies, and what happens when the disk fills. A ' +
      'directory chosen here is where the recorder writes from the next session onwards; the ' +
      'three limits below are what automatic cleanup enforces.',
    keys: [RECORDING_DIRECTORY, MAXIMUM_USAGE, MINIMUM_FREE_SPACE, MAXIMUM_AGE_DAYS],
    rows: [
      {
        label: 'Where deleted recordings wait',
        key: { name: 'trash_directory', file: 'settings.json' },
        today:
          'Beside the recordings — `D:\\Clips` becomes `D:\\Clips.trash` — unless that key says ' +
          'otherwise. The folder in use is shown above, measured; what is in it, and putting ' +
          'something back, is on the Library screen.',
        needs:
          'Issue #646 to choose the folder here. Moving a trash that already holds somebody’s ' +
          'deleted recordings is the part that needs deciding, not the field.',
      },
      {
        label: 'How long the trash keeps something',
        today:
          'Nothing. SPEC.md section 28 offers 3, 7 and 30 days or immediately, and no key ' +
          'carries the choice — so nothing expires, and no item in the trash has a date on it.',
        needs:
          'Issue #646: a retention key, and something that runs `Trash::expire` against it. ' +
          'The type and the expiry are written and nothing configures or calls them.',
      },
      {
        label: 'Per-game settings',
        today:
          'Not offered here. The settings file carries a section per game, which every game ' +
          'inherits the values above through, and it is edited by hand.',
        needs: 'Issue #63 for the per-game screen SPEC.md section 31 draws.',
      },
    ],
  },
  {
    id: HOTKEYS_SECTION,
    label: 'Hotkeys',
    lead:
      'Hotkeys are global and never per game: Windows registers a combination once for a ' +
      'process, so a per-game binding is one that could not be honoured. The recorder is that ' +
      'process. Type a combination such as Ctrl+F10, or none to leave an action bound to ' +
      'nothing; saving registers it straight away, and the table below is what Windows gave ' +
      'the recorder.',
    keys: [...HOTKEY_KEYS],
    rows: [
      {
        label: 'Choosing a combination by pressing it',
        today:
          'Not offered. A combination is typed — Ctrl+F10, Ctrl+Shift+F8 — and the recorder ' +
          'refuses anything it could not bind, naming the keys it can.',
        needs: 'Issue #54 for a control you press the combination into rather than spell out.',
      },
      {
        label: 'A combination another application owns',
        today:
          'Shown below, in the recorder’s own words: Discord, Steam and NVIDIA’s overlay all ' +
          'claim function keys, and a combination Windows would not give Clipped is a key that ' +
          'does nothing. Saving a different one above takes effect at once, and a combination ' +
          'Windows refuses leaves the action on the one it had.',
        needs: 'Issue #417 to interrupt you with it rather than waiting for you to look here.',
      },
      {
        label: 'An action nothing performs yet',
        today:
          'Also shown below, with the issue that would build it. The key is still registered and ' +
          'the press still reports itself, so it is never a key that silently does nothing.',
        needs: 'Issue #53 for the overlay, and issue #234 for the microphone.',
      },
      {
        label: 'A hotkey for one game only',
        today:
          'Not offered, and it will not be: Windows registers a combination once for a process, ' +
          'so a per-game binding could not be honoured and would be a control that did nothing.',
        needs:
          'Nothing. SPEC.md section 31 does not list hotkeys as a per-game override (issue #232).',
      },
    ],
  },
  {
    id: NOTIFICATIONS_SECTION,
    label: 'Notifications',
    lead:
      'What Clipped interrupts you for, as a Windows notification. All four are failures — ' +
      'nothing is being recorded, or a recording ended, or a key does nothing — so every one is ' +
      'on until you switch it off. They are kept in the settings file with everything else, and ' +
      'a change here reaches the next notification rather than the next launch.',
    keys: [...NOTIFICATION_KEYS],
    rows: [
      {
        label: 'Replay saved, bookmark added, screenshot taken',
        today:
          'Nothing. Only failures interrupt you, because they are the only things the recorder ' +
          'tells this window about — a toast for a replay that was saved would be a toast for ' +
          'an event nothing reports.',
        needs:
          'Issue #110, which asks for them, and an event from the recorder for each one to be ' +
          'raised from.',
      },
      {
        label: 'Quieter notifications while you are playing',
        today:
          'Not offered, and there is nothing to quieten: every category above is a failure, and ' +
          'the moment somebody most needs to know that nothing is being recorded is while they ' +
          'are in a game.',
        needs:
          'Nothing. Issue #110 asked for non-critical notifications to be held back during ' +
          'gameplay, and the set of those is empty.',
      },
    ],
  },
  {
    id: STARTUP_SECTION,
    label: 'Startup',
    lead:
      'The switch below is not a setting in the settings file: it is one Run value under this ' +
      'account that Windows reads at sign-in, and it is the same value ' +
      '`clipped-recorder start-at-login enable` writes from a terminal. Windows lists it in ' +
      'Settings > Apps > Startup too, with a switch of its own. Closing this window minimises ' +
      'it to the notification area and leaves the recorder recording; the tray’s Exit is the ' +
      'only thing that stops one. That is fixed behaviour rather than a setting.',
    keys: [],
    rows: [
      {
        label: 'Start Clipped’s window when I sign in',
        today:
          'Nothing. What has to be running at sign-in is the recorder, and the switch above ' +
          'starts it without a window: it records, watches for games and sits in the ' +
          'notification area, and this window is a client that opens when somebody opens it.',
        needs:
          'Nothing is waiting on it. A second Run value naming this application would start a ' +
          'window nobody asked for, and issue #308 deliberately built the recorder’s entry ' +
          'and not one for the window.',
      },
    ],
  },
];
