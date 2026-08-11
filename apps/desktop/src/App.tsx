import { HashRouter } from 'react-router';
import type { ReactNode } from 'react';

import { Shell } from './Shell';

/**
 * The application.
 *
 * `HashRouter` rather than `BrowserRouter`: the production window loads the
 * interface from Tauri's asset protocol as a set of files, with no server to
 * rewrite an unknown path back to `index.html`, so a reload on `/settings`
 * would 404. The fragment never reaches the protocol handler, which makes it
 * the routing mechanism that behaves the same in the browser (`npm run
 * dev:web`) and in the window (`npm run dev`).
 */
export function App(): ReactNode {
  return (
    <HashRouter>
      <Shell />
    </HashRouter>
  );
}
