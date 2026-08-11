import type { Screen } from '@clipped/shared';
import type { ReactNode } from 'react';

/** The stand-in for a screen the application does not have yet. */
export interface ScreenNotBuiltProps {
  /** The screen the sidebar navigated to. */
  readonly screen: Screen;
}

/**
 * What a navigation item leads to before its screen exists.
 *
 * It says the screen is not built and which issue builds it. It deliberately
 * does not draw an empty table, an empty library or a zeroed statistic:
 * a convincing empty state is indistinguishable from a broken one, and
 * inventing content to fill a screen is exactly what AGENTS.md section 27
 * forbids.
 */
export function ScreenNotBuilt({ screen }: ScreenNotBuiltProps): ReactNode {
  return (
    <>
      <h1 className="clipped-screen__title">{screen.label}</h1>
      <div className="clipped-unbuilt">
        <h2 className="clipped-unbuilt__heading">Not built yet</h2>
        <p className="clipped-unbuilt__body">
          The {screen.label} screen has not been written. Issue #{screen.trackedIn} builds it.
        </p>
        <p className="clipped-unbuilt__body clipped-muted">
          This is the application shell. Navigation, layout and theming are in place; the screens
          and the connection to the recorder are not.
        </p>
      </div>
    </>
  );
}
