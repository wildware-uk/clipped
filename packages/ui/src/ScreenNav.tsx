import type { Screen } from '@clipped/shared';
import type { MouseEvent, ReactNode } from 'react';

/** One navigation list in the sidebar. */
export interface ScreenNavProps {
  /**
   * What the list is, for assistive technology. A window with more than one
   * `<nav>` needs each of them named, or a screen reader announces
   * "navigation" twice and says nothing about either.
   */
  readonly label: string;
  /** The screens in the list, in order. */
  readonly screens: readonly Screen[];
  /** The path of the screen currently on show. */
  readonly currentPath: string;
  /** Turns a screen into the address shown on its link. */
  readonly hrefFor: (screen: Screen) => string;
  /** Called when a link is activated, by mouse or by keyboard. */
  readonly onNavigate: (screen: Screen) => void;
  /** `utility` draws the smaller secondary group below the rule. */
  readonly variant?: 'primary' | 'utility';
}

/**
 * A list of links to screens.
 *
 * They are real anchors with real addresses, so the keyboard reaches them by
 * tabbing and activates them with Enter, and the platform's own link handling -
 * focus ring, screen-reader role and announcement - applies without being
 * reimplemented on a `<div>`. Activation is handed to `onNavigate` rather than
 * left to the window, so that the router owns the transition and the window
 * never reloads the whole application to change screen.
 */
export function ScreenNav({
  label,
  screens,
  currentPath,
  hrefFor,
  onNavigate,
  variant = 'primary',
}: ScreenNavProps): ReactNode {
  const className = variant === 'utility' ? 'clipped-nav clipped-nav--utility' : 'clipped-nav';

  const activate = (event: MouseEvent<HTMLAnchorElement>, screen: Screen): void => {
    event.preventDefault();
    onNavigate(screen);
  };

  return (
    <nav className={className} aria-label={label}>
      <ul className="clipped-nav__list">
        {screens.map((screen) => {
          const current = screen.path === currentPath;
          return (
            <li key={screen.id}>
              <a
                className="clipped-nav__link"
                href={hrefFor(screen)}
                aria-current={current ? 'page' : undefined}
                onClick={(event) => {
                  activate(event, screen);
                }}
              >
                {screen.label}
              </a>
            </li>
          );
        })}
      </ul>
    </nav>
  );
}
