import type { JSX } from 'react';

import type { Screen } from '@clipped/shared';

export interface UnbuiltScreenProps {
    screen: Screen;
}

/**
 * What a screen shows before the feature behind it exists.
 *
 * Not a mock of the finished screen and not sample data: a statement of what
 * the screen will be and which issue builds it. AGENTS.md section 27 asks that
 * the interface represent an unavailable feature clearly rather than appear to
 * work, and section 54 rules out standing in for one with invented content.
 *
 * Each of these disappears when its screen is built. The last one to go takes
 * this component with it.
 */
export function UnbuiltScreen({ screen }: UnbuiltScreenProps): JSX.Element {
    return (
        <section className="app-unbuilt" aria-labelledby={`unbuilt-${screen.id}`}>
            <div className="app-unbuilt-title" id={`unbuilt-${screen.id}`}>
                Not built yet
            </div>
            <p>{screen.summary}</p>
            {screen.trackedBy === null ? null : (
                <p className="app-unbuilt-tracking">Tracked in issue #{screen.trackedBy}.</p>
            )}
        </section>
    );
}
