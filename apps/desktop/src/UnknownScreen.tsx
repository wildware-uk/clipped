import type { ReactNode } from 'react';
import { useLocation } from 'react-router';

/**
 * What an address that matches no screen leads to.
 *
 * Nothing in the shell links here — it is reached by editing the fragment, by a
 * stale window position restored from a previous version, or by a bug — so it
 * names the address it could not resolve, which is the one thing a bug report
 * needs to contain.
 */
export function UnknownScreen(): ReactNode {
  const { pathname } = useLocation();

  return (
    <>
      <h1 className="clipped-screen__title">Screen not found</h1>
      <div className="clipped-unbuilt">
        <p className="clipped-unbuilt__body">
          Nothing in Clipped answers to <code>{pathname}</code>.
        </p>
        <p className="clipped-unbuilt__body">
          <a href="#/">Go to Home</a>
        </p>
      </div>
    </>
  );
}
