import { SCREENS, screensInGroup, type Screen } from '@clipped/shared';
import { AppShell, RecorderStatus, ScreenNav, ScreenNotBuilt } from '@clipped/ui';
import { useCallback, type ReactNode } from 'react';
import { Route, Routes, useLocation, useNavigate } from 'react-router';

import { DiagnosticsScreen } from './DiagnosticsScreen';
import { GamesScreen } from './GamesScreen';
import { UnknownScreen } from './UnknownScreen';
import {
  describeInterruption,
  describeRecorderLink,
  useRecorderLink,
  type RecorderLinkView,
} from './useRecorderLink';
import { joinNotices, useTray } from './useTray';
import { useWindowTitle } from './useWindowTitle';

const hrefFor = (screen: Screen): string => `#${screen.path}`;

const screenFor = (pathname: string): Screen | undefined =>
  SCREENS.find((screen) => screen.path === pathname);

/**
 * What a screen's route renders.
 *
 * Every route still comes from `SCREENS`, so the sidebar and the router cannot
 * disagree about what the application contains; this is only the question of
 * which element sits behind one. Five of the seven are still the placeholder
 * that names the issue building them. Games is written (issue #107) and
 * Diagnostics is (issue #101), and the change that builds another screen adds it
 * here.
 */
function elementFor(screen: Screen, view: RecorderLinkView, notice: string | undefined): ReactNode {
  switch (screen.id) {
    case 'games':
      return <GamesScreen link={view.link} />;
    case 'diagnostics':
      return <DiagnosticsScreen view={view} notice={notice} />;
    default:
      return <ScreenNotBuilt screen={screen} />;
  }
}

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
  const view = useRecorderLink();
  const recorder = describeRecorderLink(view.link);

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

  // The tray's Open Library and Settings raise the window and send it here.
  // Routing is the shell's, not the tray's: the Rust side names a path and this
  // is the only thing that knows what to do with one.
  const goToPath = useCallback(
    (path: string) => {
      void navigate(path);
    },
    [navigate],
  );
  const trayNotice = useTray(goToPath);

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
         * The notice is the other half, and the only part of this block that is
         * not a state: when a recorder is killed mid-recording the supervisor
         * names the file it left, and ADR 0006 settled that naming it is the
         * whole of what recovery means. Dropping that here would leave the
         * window showing "Idle" and the user with a recording they cannot find.
         *
         * The notice also carries whatever the tray had to report. A tray menu
         * closes the instant it is clicked, so an action that failed has
         * nowhere of its own to say so; the window is raised carrying the
         * sentence, which is the only surface Clipped has that can hold one
         * (AGENTS.md section 45, issue #50).
         *
         * There are still no controls *here*: a "Try again" control for a link
         * that has given up is issue #221, and a Start Recording button belongs
         * with the screens that have somewhere to put it. The tray is where the
         * application is driven from until then.
         */
        <RecorderStatus
          state={recorder.state}
          detail={recorder.detail}
          notice={joinNotices(trayNotice, describeInterruption(view.interrupted))}
        />
      }
    >
      <Routes>
        {SCREENS.map((entry) => (
          <Route key={entry.id} path={entry.path} element={elementFor(entry, view, trayNotice)} />
        ))}
        <Route path="*" element={<UnknownScreen />} />
      </Routes>
    </AppShell>
  );
}
