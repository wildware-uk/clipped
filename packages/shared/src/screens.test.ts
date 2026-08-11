import { describe, expect, it } from 'vitest';

import { SCREENS, isScreenId, screenById, screensInGroup } from './screens.ts';

describe('SCREENS', () => {
  it('gives every screen a unique identifier', () => {
    const ids = SCREENS.map((screen) => screen.id);
    expect(new Set(ids).size).toBe(ids.length);
  });

  it('accounts for every screen in exactly one sidebar group', () => {
    const grouped = [...screensInGroup('primary'), ...screensInGroup('utility')];
    expect(grouped).toEqual([...SCREENS]);
  });

  it('describes every screen, so navigation reads before anything is built', () => {
    for (const { id, label, summary } of SCREENS) {
      expect(label.length, `${id} has no label`).toBeGreaterThan(0);
      expect(summary.length, `${id} has no summary`).toBeGreaterThan(0);
    }
  });

  it('looks a screen up by identifier, and refuses one that does not exist', () => {
    expect(screenById('settings').label).toBe('Settings');
    // @ts-expect-error - the point of the test is the runtime guard behind the type.
    expect(() => screenById('nowhere')).toThrow(/nowhere/);
  });

  it('recognises only real screen identifiers', () => {
    expect(isScreenId('home')).toBe(true);
    expect(isScreenId('Home')).toBe(false);
    expect(isScreenId('')).toBe(false);
  });
});
