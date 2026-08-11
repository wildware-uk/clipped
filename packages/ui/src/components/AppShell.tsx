import type { JSX, ReactNode } from 'react';

import { screenById, type ScreenId } from '@clipped/shared';

import { Sidebar } from './Sidebar.tsx';
import { TitleBar, type WindowControls } from './TitleBar.tsx';

export interface AppShellProps {
  currentScreenId: ScreenId;
  onNavigate: (id: ScreenId) => void;
  windowControls: WindowControls;
  /** The block at the foot of the sidebar. See {@link Sidebar}. */
  sidebarFooter?: ReactNode;
  /** The current screen, rendered below its heading. */
  children: ReactNode;
}

const HEADING_ID = 'app-screen-heading';
const MAIN_ID = 'app-main';

/**
 * The window's fixed structure: title bar, navigation, content.
 *
 * Everything on screen sits inside one of the three, and only the third
 * changes as the user navigates. The shell owns the heading rather than each
 * screen, so that the content region always has an accessible name and the
 * heading level is decided once instead of per screen.
 */
export function AppShell({
  currentScreenId,
  onNavigate,
  windowControls,
  sidebarFooter,
  children,
}: AppShellProps): JSX.Element {
  const screen = screenById(currentScreenId);

  return (
    <div className="app-shell">
      <a className="app-skip-link" href={`#${MAIN_ID}`}>
        Skip to content
      </a>

      <TitleBar name="Clipped" tagline="Game Recorder" windowControls={windowControls} />

      <div className="app-body">
        <Sidebar currentScreenId={currentScreenId} onNavigate={onNavigate} footer={sidebarFooter} />

        {/* `tabIndex={-1}` is what lets the skip link land here: without it the
            anchor scrolls the region into view but leaves focus where it was,
            and the next Tab goes straight back into the sidebar. */}
        <main id={MAIN_ID} className="app-main" tabIndex={-1} aria-labelledby={HEADING_ID}>
          <h1 id={HEADING_ID} className="app-screen-heading">
            {screen.label}
          </h1>
          {children}
        </main>
      </div>
    </div>
  );
}
