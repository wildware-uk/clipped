/**
 * The application's screens, as one list.
 *
 * The sidebar and the router are both built from this array rather than from
 * two lists that have to be kept in step, so a navigation item cannot point at
 * a route that does not exist.
 *
 * Three of these screens have been written — Home, Library and Games, in
 * `HomeScreen.tsx`, `LibraryScreen.tsx` and `GamesScreen.tsx`. The shell routes
 * the other four to a panel that says the screen is not written and names the
 * issue that builds it, which is why every entry carries `trackedIn`: a
 * navigation item that led to a blank screen, or to a convincing empty one,
 * would be indistinguishable from a broken application (AGENTS.md section 27).
 *
 * Which of the two a screen gets is decided in one place, `elementFor` in
 * `Shell.tsx`, rather than being recorded here. This list is what the
 * application contains; how far along each one is belongs to its issue.
 */

/** Stable identifier for a screen. Used as a React key and in tests. */
export type ScreenId =
  'home' | 'library' | 'games' | 'editor' | 'settings' | 'trash' | 'diagnostics';

/**
 * Where a screen sits in the sidebar.
 *
 * `primary` is the day-to-day set; `utility` is the smaller group below the
 * rule — the places you go to clean up or to find out what went wrong.
 */
export type ScreenGroup = 'primary' | 'utility';

/** One destination in the application shell. */
export interface Screen {
  /** Stable identifier, independent of the label and the route. */
  readonly id: ScreenId;
  /** Sidebar label, in sentence case, as the design deck writes it. */
  readonly label: string;
  /** Route path, without the leading hash the router adds. */
  readonly path: string;
  /** Which sidebar group the screen belongs to. */
  readonly group: ScreenGroup;
  /**
   * The GitHub issue that builds the screen.
   *
   * While a screen is unwritten the placeholder names this issue, so somebody
   * who lands on it can go and read what is planned. The number stays after the
   * screen is written: it is still where the screen came from, and the issue is
   * still where the rest of it is tracked.
   */
  readonly trackedIn: number;
}

/** Every screen in the shell, in sidebar order. */
export const SCREENS: readonly Screen[] = [
  {
    id: 'home',
    label: 'Home',
    path: '/',
    group: 'primary',
    trackedIn: 60,
  },
  {
    id: 'library',
    label: 'Library',
    path: '/library',
    group: 'primary',
    trackedIn: 60,
  },
  {
    id: 'games',
    label: 'Games',
    path: '/games',
    group: 'primary',
    trackedIn: 107,
  },
  {
    id: 'editor',
    label: 'Editor',
    path: '/editor',
    group: 'primary',
    trackedIn: 83,
  },
  {
    id: 'settings',
    label: 'Settings',
    path: '/settings',
    group: 'primary',
    trackedIn: 51,
  },
  {
    id: 'trash',
    label: 'Trash',
    path: '/trash',
    group: 'utility',
    trackedIn: 94,
  },
  {
    id: 'diagnostics',
    label: 'Diagnostics',
    path: '/diagnostics',
    group: 'utility',
    trackedIn: 101,
  },
];

/** The screens in one sidebar group, in order. */
export function screensInGroup(group: ScreenGroup): readonly Screen[] {
  return SCREENS.filter((screen) => screen.group === group);
}
