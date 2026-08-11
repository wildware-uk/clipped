/**
 * The destinations the desktop application's navigation offers.
 *
 * This lives in `@clipped/shared` rather than in either package that uses it
 * because both do: `@clipped/ui` renders the navigation from it and knows
 * nothing about which screen is showing, while `apps/desktop` owns the current
 * screen and mounts the view for it.
 */

/** The identifier a screen is addressed by, in navigation and in routing. */
export type ScreenId =
  'home' | 'library' | 'games' | 'editor' | 'settings' | 'trash' | 'diagnostics';

/**
 * Which block of the sidebar a screen appears in.
 *
 * `primary` is the application's own work; `utility` is the housekeeping below
 * the divider. The design deck draws them as two lists separated by a rule.
 */
export type ScreenGroup = 'primary' | 'utility';

/** A navigable screen, and the state of the feature behind it. */
export interface Screen {
  readonly id: ScreenId;
  /** The label shown in navigation and used as the screen's heading. */
  readonly label: string;
  readonly group: ScreenGroup;
  /**
   * One sentence describing what the screen is for, shown while it is unbuilt
   * so that the navigation reads as more than a list of nouns.
   */
  readonly summary: string;
  /**
   * The GitHub issue that builds this screen, or `null` once it is built.
   *
   * The shell renders an honest "not built yet" notice for every screen that
   * still carries an issue number, rather than a mock of the finished screen
   * (AGENTS.md sections 27 and 54). Clearing this field is part of the work of
   * the issue it names.
   */
  readonly trackedBy: number | null;
}

/**
 * Every screen, in the order the sidebar lists them.
 *
 * The order is the design deck's: the recorder first, then what it produced,
 * then what it is configured by, with housekeeping last.
 */
export const SCREENS: readonly Screen[] = [
  {
    id: 'home',
    label: 'Home',
    group: 'primary',
    summary:
      'What the recorder is doing now, and the sessions and clips it produced most recently.',
    trackedBy: 60,
  },
  {
    id: 'library',
    label: 'Library',
    group: 'primary',
    summary: 'Every session, clip, highlight and screenshot, searchable and filterable.',
    trackedBy: 60,
  },
  {
    id: 'games',
    label: 'Games',
    group: 'primary',
    summary: 'The games Clipped has detected, and how each one is recorded.',
    trackedBy: 107,
  },
  {
    id: 'editor',
    label: 'Editor',
    group: 'primary',
    summary: 'Trim, split and mix a clip without re-encoding the recording it came from.',
    trackedBy: 83,
  },
  {
    id: 'settings',
    label: 'Settings',
    group: 'primary',
    summary: 'Recording, audio, storage, hotkey and interface settings.',
    trackedBy: 51,
  },
  {
    id: 'trash',
    label: 'Trash',
    group: 'utility',
    summary: 'Deleted recordings, kept for a retention period so a deletion can be undone.',
    trackedBy: 94,
  },
  {
    id: 'diagnostics',
    label: 'Diagnostics',
    group: 'utility',
    summary: 'Capture, encoder, audio and storage measurements from the running recorder.',
    trackedBy: 101,
  },
];

/** The screens in one sidebar block, in navigation order. */
export function screensInGroup(group: ScreenGroup): readonly Screen[] {
  return SCREENS.filter((screen) => screen.group === group);
}

/** The screen with this identifier. Throws if it does not exist. */
export function screenById(id: ScreenId): Screen {
  const screen = SCREENS.find((candidate) => candidate.id === id);
  if (!screen) {
    throw new Error(`No screen is registered with the identifier "${id}".`);
  }
  return screen;
}

/**
 * Whether an arbitrary string names a screen.
 *
 * Used where a screen identifier arrives from outside the type system - a
 * restored window state, a deep link from the tray - and has to be rejected
 * rather than trusted.
 */
export function isScreenId(value: string): value is ScreenId {
  return SCREENS.some((screen) => screen.id === value);
}
