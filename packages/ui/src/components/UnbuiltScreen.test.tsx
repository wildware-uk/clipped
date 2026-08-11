import { screenById, type Screen } from '@clipped/shared';
import { render, screen } from '@testing-library/react';
import { describe, expect, it } from 'vitest';

import { UnbuiltScreen } from './UnbuiltScreen.tsx';

describe('UnbuiltScreen', () => {
  it('says what the screen will be and which issue builds it', () => {
    const games = screenById('games');
    render(<UnbuiltScreen screen={games} />);

    expect(screen.getByText('Not built yet')).toBeDefined();
    expect(screen.getByText(games.summary)).toBeDefined();
    expect(screen.getByText(`Tracked in issue #${games.trackedBy}.`)).toBeDefined();
  });

  it('drops the tracking line once a screen is no longer waiting on an issue', () => {
    const built: Screen = { ...screenById('games'), trackedBy: null };
    render(<UnbuiltScreen screen={built} />);

    expect(screen.queryByText(/Tracked in issue/)).toBeNull();
  });
});
