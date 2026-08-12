import { SCREENS, screensInGroup, type Screen } from '@clipped/shared';
import { AppShell, RecorderStatus, ScreenNav, ScreenNotBuilt } from '@clipped/ui';
import { useCallback, type ReactNode } from 'react';
import { Link, Route, Routes, useLocation, useNavigate } from 'react-router';

import { ClipPlaybackScreen } from './ClipPlaybackScreen';
import { CLIP_ROUTE, clipPath, isClipPath } from './clipPlayback';
import { DiagnosticsScreen } from './DiagnosticsScreen';
import { GamesScreen } from './GamesScreen';
import { HomeScreen } from './HomeScreen';
import { LibraryScreen } from './LibraryScreen';
import { SettingsScreen } from './SettingsScreen';
import { UnknownScreen } from './UnknownScreen';
import {
  describeInterruption,
  describeRecorderLink,
  useRecorderLink,
  type InterruptedRecording,
  type RecorderLinkView,
} from './useRecorderLink';
import { joinNotices, useTray } from './useTray';
import { useWindowTitle } from './useWindowTitle';

const hrefFor = (screen: Screen): string => `#${screen.path}`;

const screenFor = (pathname: string): Screen | undefined =>
  SCREENS.find((screen) => screen.path === pathname);

/**
 * The window's title for a path.
 *
 * `SCREENS` answers for the seven sidebar destinations. The playback screen is
 * the eighth route and deliberately not one of them — it is opened for a
 * particular recording rather than navigated to (issue #52) — so it is named
 * here. Without this the taskbar, Alt+Tab and the screen reader's window
 * announcement would all say "Clipped" and nothing else, on the one screen
 * whose subject is a single file.
 */
function titleFor(pathname: string, screen: Screen | undefined): string {
  if (screen !== undefined) {
    return `Clipped — ${screen.label}`;
  }
  return isClipPath(pathname) ? 'Clipped — Playback' : 'Clipped';
}

/**
 * What a screen's route renders.
 *
 * Every route still comes from `SCREENS`, so the sidebar and the router cannot
 * disagree about what the application contains; this is only the question of
 * which element sits behind one. Five of the seven are written — Home and Library
 * (issue #60), Games (issue #107), Settings (issue #51) and Diagnostics
 * (issue #101) — and the other two are still the placeholder that names the issue
 * building them. The change that builds another screen adds it here, which is the
 * one place that knows a screen from a placeholder.
 */
function elementFor(screen: Screen, view: RecorderLinkView, notice: string | undefined): ReactNode {
  switch (screen.id) {
    case 'home':
      return <HomeScreen link={view.link} />;
    case 'library':
      return <LibraryScreen />;
    case 'games':
      return <GamesScreen link={view.link} />;
    case 'settings':
      return <SettingsScreen />;
    case 'diagnostics':
      return <DiagnosticsScreen view={view} notice={notice} />;
    default:
      return <ScreenNotBuilt screen={screen} />;
  }
}

/**
 * The sidebar's notice, with a way in to the recording it is about.
 *
 * `describeInterruption` is still the sentence — it is the wording ADR 0006's
 * recovery produces, and it stays a tested pure function of the interruption —
 * and this adds the one destination that goes with it. A recorder that died
 * mid-recording is the only recording this window can name (`clipPlayback.ts`),
 * so it is the only way in the playback screen has until the library index can
 * be reached (issue #305).
 *
 * The link does not claim the recording will play. It leads to the screen that
 * says what state the recording is in and what stands between this window and
 * playing it — a thing that happens, rather than a control that does nothing
 * (AGENTS.md section 27), which is the same bargain the tray's Open Library
 * keeps.
 */
function noticeFor(
  trayNotice: string | undefined,
  interrupted: InterruptedRecording | null,
): ReactNode {
  if (interrupted === null) {
    return trayNotice;
  }

  return (
    <>
      {joinNotices(trayNotice, describeInterruption(interrupted))}{' '}
      <Link to={clipPath(interrupted.recording_id)}>Open this recording</Link>
    </>
  );
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
  useWindowTitle(titleFor(pathname, screen));

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
         * There is still no *control* here: a "Try again" control for a link
         * that has given up is issue #221, and a Start Recording button belongs
         * with the screens that have somewhere to put it. The tray is where the
         * application is driven from until then. The one link the notice can
         * carry leads to the interrupted recording's own screen, which says
         * more about it than a sidebar has room for (issue #52) — a destination
         * rather than an action.
         */
        <RecorderStatus
          state={recorder.state}
          detail={recorder.detail}
          notice={noticeFor(trayNotice, view.interrupted)}
        />
      }
    >
      <Routes>
        {SCREENS.map((entry) => (
          <Route key={entry.id} path={entry.path} element={elementFor(entry, view, trayNotice)} />
        ))}
        {/*
         * The eighth route, and the only one not in `SCREENS`: a screen opened
         * for a particular recording rather than a destination in the sidebar,
         * so putting it in that array would put "Playback" in the navigation
         * with no recording to show (issue #52).
         */}
        <Route path={CLIP_ROUTE} element={<ClipPlaybackScreen view={view} />} />
        <Route path="*" element={<UnknownScreen />} />
      </Routes>
    </AppShell>
  );
}
