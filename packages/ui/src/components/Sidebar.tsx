import type { JSX, ReactNode } from 'react';

import { screensInGroup, type Screen, type ScreenId } from '@clipped/shared';

export interface SidebarProps {
  /** The screen currently mounted in the content region. */
  currentScreenId: ScreenId;
  onNavigate: (id: ScreenId) => void;
  /**
   * The block at the foot of the sidebar. The design puts recorder status and
   * its two actions there; until the recorder is reachable (issue #49) the
   * application passes what it can honestly say.
   */
  footer?: ReactNode;
}

/**
 * The application's primary navigation.
 *
 * Two lists separated by a rule, as the design deck draws them: the screens
 * that do the work, then the housekeeping ones. Each entry is a real `button`
 * inside a list, so a screen reader announces the group and its size, and the
 * keyboard reaches every entry with Tab alone - no roving tabindex, no key
 * handler of our own, nothing to get wrong.
 */
export function Sidebar({ currentScreenId, onNavigate, footer }: SidebarProps): JSX.Element {
  return (
    <nav className="app-sidebar" aria-label="Screens">
      <ScreenList
        className="app-nav-list-primary"
        screens={screensInGroup('primary')}
        currentScreenId={currentScreenId}
        onNavigate={onNavigate}
      />

      <div className="app-nav-divider" />

      <ScreenList
        className="app-nav-list-utility"
        screens={screensInGroup('utility')}
        currentScreenId={currentScreenId}
        onNavigate={onNavigate}
      />

      <div className="app-sidebar-spacer" />

      {footer ? <div className="app-sidebar-footer">{footer}</div> : null}
    </nav>
  );
}

interface ScreenListProps {
  className: string;
  screens: readonly Screen[];
  currentScreenId: ScreenId;
  onNavigate: (id: ScreenId) => void;
}

function ScreenList({
  className,
  screens,
  currentScreenId,
  onNavigate,
}: ScreenListProps): JSX.Element {
  return (
    <ul className={`app-nav-list ${className}`}>
      {screens.map((screen) => {
        const isCurrent = screen.id === currentScreenId;
        return (
          <li key={screen.id}>
            <button
              type="button"
              className="app-nav-item"
              // `page` rather than `true`: this is the document the user is
              // on, which is what a screen reader announces as "current page".
              aria-current={isCurrent ? 'page' : undefined}
              onClick={() => {
                onNavigate(screen.id);
              }}
            >
              {screen.label}
            </button>
          </li>
        );
      })}
    </ul>
  );
}
