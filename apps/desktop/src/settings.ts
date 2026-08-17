import type { AudioDevices, SettingEntry, SettingsView } from '@clipped/shared';
import { invoke } from '@tauri-apps/api/core';
import { open } from '@tauri-apps/plugin-dialog';
import { useCallback, useEffect, useState } from 'react';

import { asProblem, type LibraryProblem, type LibraryRead } from './library';

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

/** Where Clipped keeps a value, when anything keeps it. */
export type SettingsFile = 'settings.json' | 'notifications.json';

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

/** The notification switches, which are a second store until issue #252. */
export const NOTIFICATIONS_FILE = String.raw`%APPDATA%\uk.wildware.clipped\notifications.json`;

/**
 * The one section with something live in it beyond the settings themselves.
 *
 * Named here rather than spelled in the screen, so that renaming the section
 * cannot silently stop the hotkey list being drawn.
 */
export const HOTKEYS_SECTION = 'hotkeys';

/** The key the recording directory has in the settings file. */
export const RECORDING_DIRECTORY = 'recording_directory';

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

/** The microphones this machine has, read once when the screen is opened. */
export function useAudioDevices(): LibraryRead<AudioDevices> {
  const [read, setRead] = useState<LibraryRead<AudioDevices>>({ state: 'reading' });

  useEffect(() => {
    let current = true;
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
    return () => {
      current = false;
    };
  }, []);

  return read;
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
    lead:
      'Where recordings go, and what happens when the disk fills. A directory chosen here is ' +
      'where the recorder writes from the next recording onwards.',
    keys: [RECORDING_DIRECTORY],
    rows: [
      {
        label: 'Maximum usage, minimum free space, maximum age',
        key: { name: 'maximum_usage_bytes', file: 'settings.json' },
        today:
          `Set by hand in the storage section of ${SETTINGS_FILE}. Clipped measures what the ` +
          'library occupies against them and reports what a sweep would delete.',
        needs: 'Issue #95 for the screen SPEC.md section 27 draws them on.',
      },
      {
        label: 'Where deleted recordings wait',
        key: { name: 'trash_directory', file: 'settings.json' },
        today:
          'Beside the recordings — `D:\\Clips` becomes `D:\\Clips.trash` — unless that key says ' +
          'otherwise. What is in it, and putting something back, is on the Library screen.',
        needs: 'Issue #95, with the limits above.',
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
      'process, and the table below is what it registered when it started.',
    keys: [],
    rows: [
      {
        label: 'Which combination an action has',
        today:
          `The hotkeys section of ${SETTINGS_FILE}, read once when the recorder starts. Two ` +
          'actions are bound out of the box — Ctrl+F10 to save a replay and Ctrl+F9 to bookmark ' +
          '— and editing the file takes effect the next time Clipped starts.',
        needs:
          'Issue #54 for the screen that binds a combination, and issue #233 to change one ' +
          'without restarting the recorder.',
      },
      {
        label: 'A combination another application owns',
        today:
          'Shown below, in the recorder’s own words: Discord, Steam and NVIDIA’s overlay all ' +
          'claim function keys, and a combination Windows would not give Clipped is a key that ' +
          'does nothing. Choosing another one means editing the file above.',
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
    id: 'notifications',
    label: 'Notifications',
    lead:
      `The one thing on this screen still kept somewhere else: each switch is a key in ` +
      `${NOTIFICATIONS_FILE}, alongside "version": 1, and it is changed by hand. Every category ` +
      'is on until that file says otherwise, because all four are failures.',
    keys: [],
    rows: [
      {
        label: 'A recording failed',
        key: { name: 'recording_failed', file: 'notifications.json' },
        today: 'A recording ended because something went wrong and the recorder is still running.',
        run: '"recording_failed": false',
        needs:
          'Issue #252, which moves these four into the settings file and puts their switches on ' +
          'this screen with the rest.',
      },
      {
        label: 'A recording was interrupted',
        key: { name: 'recording_interrupted', file: 'notifications.json' },
        today:
          'A recorder stopped mid-recording without being asked. The notification names the ' +
          'file it left, which nothing else will.',
        run: '"recording_interrupted": false',
        needs: 'Issue #252, as above.',
      },
      {
        label: 'The recorder cannot be reached',
        key: { name: 'recorder_unavailable', file: 'notifications.json' },
        today:
          'The link gave up: nothing is being recorded, and nothing further will be tried on ' +
          'its own.',
        run: '"recorder_unavailable": false',
        needs: 'Issue #252, as above.',
      },
      {
        label: 'A hotkey is unavailable',
        key: { name: 'hotkey_unavailable', file: 'notifications.json' },
        today:
          'Windows refused one of Clipped’s combinations, so pressing it does nothing. Said ' +
          'once when it is first seen, not again every time the recorder reconnects.',
        run: '"hotkey_unavailable": false',
        needs: 'Issue #252, as above.',
      },
    ],
  },
  {
    id: 'startup',
    label: 'Startup',
    lead:
      'Closing this window minimises it to the notification area and leaves the recorder ' +
      'recording; the tray’s Exit is the only thing that stops one. That is fixed behaviour ' +
      'rather than a setting.',
    keys: [],
    rows: [
      {
        label: 'Start the recorder when I sign in',
        today:
          'Opt-in and reversible from the command line. It writes one Run value under this ' +
          'account, which Windows also lists in Settings > Apps > Startup; disable removes it ' +
          'and status reports it without changing anything.',
        run: 'clipped-recorder start-at-login enable',
        needs:
          'Issue #308: starting at sign-in is not configuration and no protocol command reads ' +
          'or sets it, so this window cannot offer the switch.',
      },
      {
        label: 'Start Clipped when I sign in',
        today:
          'Nothing. Starting at sign-in is the recorder’s, because the recorder is what records ' +
          'and this window is a client of it.',
        needs: 'Issue #308, with the setting above.',
      },
    ],
  },
];
