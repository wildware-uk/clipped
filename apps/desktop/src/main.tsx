import '@clipped/ui/styles.css';

import { StrictMode } from 'react';
import { createRoot } from 'react-dom/client';

import { App } from './App';

const container = document.getElementById('root');

if (!container) {
  // index.html is part of this application, so this cannot happen without the
  // document having been replaced. Say which element is missing rather than
  // letting `createRoot` report `null`.
  throw new Error('The document has no #root element for the Clipped interface to mount into.');
}

createRoot(container).render(
  <StrictMode>
    <App />
  </StrictMode>,
);
