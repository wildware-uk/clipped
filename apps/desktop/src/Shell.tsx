import { SCREENS, screensInGroup, type Screen } from '@clipped/shared';
import { AppShell, RecorderStatus, ScreenNav, ScreenNotBuilt } from '@clipped/ui';
import { useCallback, type ReactNode } from 'react';
import { Route, Routes, useLocation, useNavigate } from 'react-router';

import { UnknownScreen } from './UnknownScreen';
import { describeRecorderLink, useRecorderLink } from './useRecorderLink';
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
  const recorder = describeRecorderLink(useRecorderLink());

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
         * The state the Rust side reports, and nothing more. Every wording comes
         * from `describeRecorderLink`, which has one rendering for each of the
         * link's four states and one for "this is not the Clipped window" — so
         * nothing here can show a state the application does not have
         * (AGENTS.md section 27, issue #106).
         *
         * There are still no controls: a "Try again" control for a link that
         * has given up is issue #212, and a Start Recording button belongs with
         * the screens that have somewhere to put it.
         */
        <RecorderStatus state={recorder.state} detail={recorder.detail} />
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
