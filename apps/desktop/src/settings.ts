/**
 * What the Settings screen says, as data (issue #51).
 *
 * # Why this screen is an account of settings rather than a form
 *
 * SPEC.md sections 10, 12, 15 and 27 draw a settings screen with devices,
 * directories, presets and switches, and the application deck draws the same
 * one. **This window can neither read nor write a single setting**, so none of
 * those controls is drawn (AGENTS.md section 27). The reasoning is short and it
 * is worth having in one place:
 *
 * - the settings themselves are `clipped_session::config`, which resolves the
 *   global and per-game layers, validates every value and owns
 *   `%LOCALAPPDATA%\Clipped\settings.json` (`docs/configuration.md`). It reports
 *   each setting's value, which layer supplied it and whether this scope
 *   overrode it, which is exactly what a settings screen needs — and the window
 *   has no way to ask;
 * - the desktop application may link one crate of the repository's workspace,
 *   `clipped-ipc`, and `tests/integration/tests/workspace_layering.rs` enforces
 *   it. `clipped-session` sits above capture, audio, encoding and muxing, so
 *   naming it here would put the recording engine in the window's process — the
 *   separation ADR 0002 exists to make;
 * - the control protocol has no command that reads configuration, and its one
 *   command that would write it, `apply_settings`, is refused by every build as
 *   not implemented (`crates/ipc/src/command.rs`);
 * - reading the file from the window instead would be a second implementation
 *   of its versioning, migration and validation, against the file the user's
 *   settings live in (AGENTS.md section 55).
 *
 * [Issue #252](https://github.com/wildware-uk/clipped/issues/252) is the one
 * that fixes this, by either route, and it says it blocks this screen.
 *
 * # What is drawn instead
 *
 * For every setting: how it is set **today**, and what has to land before this
 * window can hold it. A user leaves this screen able to change the thing they
 * came for — from a command line or, for the notification switches, from a file
 * — which is the useful action AGENTS.md section 45 asks for and which an empty
 * form with a disabled Save button would not have given them.
 *
 * Two claims are the load-bearing ones, and `settingsConformance.test.ts`
 * checks both against the Rust that owns them rather than against this file: the
 * settings named here are exactly the ones the configuration API models, and the
 * commands named here are subcommands the recorder actually has.
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

/** One setting, and where it stands. */
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
  /** What the section is about, and which file carries it. */
  readonly lead: string;
  /** The settings in it. */
  readonly rows: readonly SettingRow[];
}

/** Clipped's settings file, as a user would find it. */
export const SETTINGS_FILE = String.raw`%LOCALAPPDATA%\Clipped\settings.json`;

/** The notification switches, which are a second store until issue #252. */
export const NOTIFICATIONS_FILE = String.raw`%APPDATA%\uk.wildware.clipped\notifications.json`;

/**
 * The one section with something live in it.
 *
 * Named here rather than spelled in the screen, so that renaming the section
 * cannot silently stop the hotkey list being drawn — the list would simply
 * vanish, and a section that quietly lost its only real content is exactly the
 * kind of regression nobody notices.
 */
export const HOTKEYS_SECTION = 'hotkeys';

/**
 * Why no control on this screen changes anything, in the words the screen says
 * it in.
 *
 * One statement rather than a disabled control beside each setting: a screen
 * whose every row is greyed out says the same thing forty times and none of the
 * times says why.
 */
export const NOTHING_IS_EDITABLE = {
  heading: 'No setting can be changed from this window',
  /* The mechanism, because "coming soon" is not a reason and cannot be acted
     on. */
  why:
    `The recorder owns ${SETTINGS_FILE}. The control protocol has no command that reads it, and ` +
    'the one that would write it, apply_settings, is refused as not implemented by every build. ' +
    'Issue #252 makes the configuration API reachable from this window.',
  /* And what the screen does instead, so that the sections below are read as
     what they are. */
  instead:
    'Each section names how a setting is set today, and the work that has to land before this ' +
    'window can hold the control. Nothing here is drawn as a control that would do nothing.',
} as const;

/**
 * Every section of the screen, and every setting in it.
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
      `What a recording is made at. The settings shown with a key are the ones ${SETTINGS_FILE} ` +
      'carries, and nothing reads them when a recording starts: what a recording is actually ' +
      'made with comes from the command line that started the recorder (issue #61).',
    rows: [
      {
        label: 'Capture target',
        key: { name: 'capture_target', file: 'settings.json' },
        today:
          'The game’s own window. There is deliberately no capture-mode option on the command ' +
          'line: Full Session is the only mode this build runs.',
        run: 'clipped-recorder watch',
        needs:
          'Issue #61 to read the setting when a recording starts, and issue #252 to reach it ' +
          'from here.',
      },
      {
        label: 'Resolution',
        key: { name: 'resolution', file: 'settings.json' },
        today:
          'Whatever the source produces. There is no scaler between capture and the encoder, so ' +
          'a fixed size that is not the source’s is refused when the recording starts rather ' +
          'than quietly ignored.',
        run: 'clipped-recorder watch --resolution source',
        needs: 'Issues #61 and #252, as above.',
      },
      {
        label: 'Frame rate',
        key: { name: 'framerate', file: 'settings.json' },
        today: 'A ceiling rather than a pace, from 1 to 480, applied to every recording that run.',
        run: 'clipped-recorder watch --framerate 60',
        needs: 'Issues #61 and #252, as above.',
      },
      {
        label: 'Codec',
        key: { name: 'codec', file: 'settings.json' },
        today:
          'auto, h264, hevc or av1. auto picks from what this machine can open, which ' +
          'clipped-recorder capabilities reports.',
        run: 'clipped-recorder watch --codec auto',
        needs: 'Issues #61 and #252, as above.',
      },
      {
        label: 'Encoder',
        key: { name: 'encoder', file: 'settings.json' },
        today: 'auto, nvenc, amf, quicksync or software, chosen the same way as the codec.',
        run: 'clipped-recorder watch --encoder auto',
        needs: 'Issues #61 and #252, as above.',
      },
      {
        label: 'Replay window',
        key: { name: 'replay_window_seconds', file: 'settings.json' },
        today:
          'Nothing. The replay buffer is written and no build starts a recording that runs one, ' +
          'so there is no buffer for a length to apply to.',
        needs:
          'Issue #38 to run a recording with a replay buffer, then issues #61 and #252 to ' +
          'configure its length from here.',
      },
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
          'nothing calls it, so there is no format to choose between yet.',
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
      'A recording has no audio track at all yet (issue #180). These are the settings for ' +
      'something that is not yet recorded, which is why the recorder warns when it is given one.',
    rows: [
      {
        label: 'Microphone',
        key: { name: 'microphone', file: 'settings.json' },
        today:
          'default, none, or part of a device name, matched against the endpoints present when ' +
          'a recording starts. A recording carries no audio, so it changes nothing today.',
        run: 'clipped-recorder watch --microphone default',
        needs:
          'Issue #180 to record an audio track, issue #308 for a way to list this machine’s ' +
          'devices to choose from, and issue #252 to save the choice.',
      },
      {
        label: 'System audio',
        key: { name: 'system_audio', file: 'settings.json' },
        today: 'The same, for the endpoint the machine plays through.',
        run: 'clipped-recorder watch --system-audio default',
        needs: 'Issues #180, #308 and #252, as above.',
      },
      {
        label: 'Audio tracks, enable and level',
        today:
          'Nothing. The muxer writes separate game, microphone and system tracks, and nothing ' +
          'produces the tracks for it to write.',
        needs:
          'Issue #180 first, then issue #81 for the track list SPEC.md section 12 draws and ' +
          'issue #33 for routing an application to a track.',
      },
    ],
  },
  {
    id: 'storage',
    label: 'Storage',
    lead: `Where recordings go, and what happens when the disk fills. ${SETTINGS_FILE} carries none of this.`,
    rows: [
      {
        label: 'Recording directory',
        today:
          'Named per run on the command line. Without one, recordings and session records go ' +
          'to the Clipped folder of your videos directory, which is created when the recorder ' +
          'starts rather than when a game launches.',
        run: String.raw`clipped-recorder watch --output-directory D:\clips`,
        needs:
          'Issue #307: the settings file has no key for the recording directory, so there is ' +
          'nowhere for this window to save one.',
      },
      {
        label: 'Maximum usage, minimum free space, maximum age',
        today:
          'Nothing sets them. Clipped measures what the library occupies and whether limits are ' +
          'met, and deletes nothing at all.',
        needs:
          'Issue #307 for the keys, issue #111 to act on a breached limit, and issue #95 for ' +
          'the screen SPEC.md section 27 draws.',
      },
      {
        label: 'Trash and recovery',
        today:
          'Nothing. Clipped deletes no recording of its own, and there is no trash to recover ' +
          'from if something else does.',
        needs: 'Issue #94.',
      },
    ],
  },
  {
    id: HOTKEYS_SECTION,
    label: 'Hotkeys',
    lead:
      'Hotkeys are global and never per game: Windows registers a combination once for a ' +
      'process, so a per-game binding is one that could not be honoured. The recorder is that ' +
      'process, and the table above is what it registered when it started.',
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
          'Shown above, in the recorder’s own words: Discord, Steam and NVIDIA’s overlay all ' +
          'claim function keys, and a combination Windows would not give Clipped is a key that ' +
          'does nothing. Choosing another one means editing the file above.',
        needs: 'Issue #417 to interrupt you with it rather than waiting for you to look here.',
      },
      {
        label: 'An action nothing performs yet',
        today:
          'Also shown above, with the milestone and issue that would build it. The key is still ' +
          'registered and the press still reports itself, so it is never a key that silently ' +
          'does nothing: saving a replay waits on issue #38, and the overlay on issue #53.',
        needs:
          'Issue #38 for the replay buffer, issue #53 for the overlay, issue #234 for the microphone.',
      },
      {
        label: 'Starting a recording from a key',
        today:
          'Not possible. Bound, the start-or-stop key stops the recording that is running; with ' +
          'nothing recording it refuses, because a key press does not say which window to record.',
        needs: 'Issue #416.',
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
      `The only settings Clipped keeps between runs today, and they are changed by hand: each ` +
      `switch is a key in ${NOTIFICATIONS_FILE}, alongside "version": 1. Every category is on ` +
      'until that file says otherwise, because all three are failures. A file Clipped cannot ' +
      'read is reported when the window opens rather than ignored.',
    rows: [
      {
        label: 'A recording failed',
        key: { name: 'recording_failed', file: 'notifications.json' },
        today: 'A recording ended because something went wrong and the recorder is still running.',
        run: '"recording_failed": false',
        needs:
          'Issue #252, which moves these three into the settings file and puts their switches ' +
          'on this screen.',
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
    ],
  },
  {
    id: 'startup',
    label: 'Startup',
    lead:
      'Closing this window minimises it to the notification area and leaves the recorder ' +
      'recording; the tray’s Exit is the only thing that stops one. That is fixed behaviour ' +
      'rather than a setting.',
    rows: [
      {
        label: 'Start the recorder when I sign in',
        today:
          'Opt-in and reversible from the command line. It writes one Run value under this ' +
          'account, which Windows also lists in Settings > Apps > Startup; disable removes it ' +
          'and status reports it without changing anything.',
        run: 'clipped-recorder start-at-login enable',
        needs:
          'Issue #308: no protocol command reads or sets it, so this window cannot offer the ' +
          'switch.',
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
