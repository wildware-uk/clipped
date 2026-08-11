import '@clipped/ui/styles.css';
import './app.css';

import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';

import { App } from './App.tsx';

const container = document.getElementById('root');
if (!container) {
  // index.html is ours and is bundled with the application, so this can only
  // happen if the document was replaced. Failing loudly beats rendering into
  // a detached node and showing an empty window with no explanation.
  throw new Error('The document has no #root element to mount the application into.');
}

createRoot(container).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
