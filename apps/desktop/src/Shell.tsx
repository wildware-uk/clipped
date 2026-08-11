import { SCREENS, screensInGroup, type Screen } from '@clipped/shared';
import { AppShell, RecorderStatus, ScreenNav, ScreenNotBuilt } from '@clipped/ui';
import { useCallback, type ReactNode } from 'react';
import { Route, Routes, useLocation, useNavigate } from 'react-router';

import { UnknownScreen } from './UnknownScreen';
import { useWindowTitle } from './useWindowTitle';

const hrefFor = (screen: Screen): string => `#${screen.path}`;

const screenFor = (pathname: string): Screen | undefined =>
  SCREENS.find((screen) => screen.path === pathname);

/**
 * The shell: the chrome that stays put, and the screen that does not.
 *
 * Both navigation lists and every route come from `SCREENS`, so the sidebar and
 * the router cannot disagree about what the application contains.
 */
export function Shell(): ReactNode {
  const { pathname } = useLocation();
  const navigate = useNavigate();
  const screen = screenFor(pathname);

  // The window title is what a person reads in the taskbar, in Alt+Tab and in
  // the window switcher, so it says which screen is open rather than only which
  // application it is.
  useWindowTitle(screen ? `Clipped — ${screen.label}` : 'Clipped');

  const goTo = useCallback(
    (destination: Screen) => {
      void navigate(destination.path);
    },
    [navigate],
  );

  return (
    <AppShell
      screenKey={pathname}
      nav={
        <>
          <ScreenNav
            label="Screens"
            screens={screensInGroup('primary')}
            currentPath={pathname}
            hrefFor={hrefFor}
            onNavigate={goTo}
          />
          <div className="clipped-sidebar__rule" />
          <ScreenNav
            label="Maintenance"
            screens={screensInGroup('utility')}
            currentPath={pathname}
            hrefFor={hrefFor}
            onNavigate={goTo}
            variant="utility"
          />
        </>
      }
      status={
        /*
         * The truthful state, and the only one the shell can be in today: there
         * is no IPC protocol yet (issue #49), so the application has no way to
         * ask the recorder anything. Showing "Idle", a timer or a level meter
         * here would be invented (AGENTS.md section 27).
         */
        <RecorderStatus
          state="Not connected"
          detail="This window cannot talk to the recorder yet. Record with the clipped-recorder command line in the meantime."
        />
      }
    >
      <Routes>
        {SCREENS.map((entry) => (
          // Every screen is a placeholder, because none of them has been
          // written. The change that builds one swaps its element here.
          <Route key={entry.id} path={entry.path} element={<ScreenNotBuilt screen={entry} />} />
        ))}
        <Route path="*" element={<UnknownScreen />} />
      </Routes>
    </AppShell>
  );
}
