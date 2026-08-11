import { screenById, type ScreenId } from '@clipped/shared';
import { AppShell, UnbuiltScreen } from '@clipped/ui';
import { useState, type JSX } from 'react';

import { RecorderStatus } from './RecorderStatus.tsx';
import { useWindowControls } from './useWindowControls.ts';

/**
 * The desktop application.
 *
 * Navigation is a screen identifier in state rather than a router: this is a
 * window with a fixed set of destinations and no address bar, no history and
 * nothing to deep-link to, so a routing library would add a dependency and a
 * URL model that nothing here would use (AGENTS.md section 10). When deep
 * links do arrive - the tray menu opening a screen, a notification opening a
 * session - they set this same state.
 *
 * Every screen is still unbuilt, so every one renders the same honest notice.
 * As each is built it takes its own branch here.
 */
export function App(): JSX.Element {
  const [currentScreenId, setCurrentScreenId] = useState<ScreenId>('home');
  const windowControls = useWindowControls();

  return (
    <AppShell
      currentScreenId={currentScreenId}
      onNavigate={setCurrentScreenId}
      windowControls={windowControls}
      sidebarFooter={<RecorderStatus />}
    >
      <UnbuiltScreen screen={screenById(currentScreenId)} />
    </AppShell>
  );
}
