import { render, screen } from '@testing-library/react';
import { describe, expect, it, vi } from 'vitest';

import { AppShell } from './AppShell.tsx';
import type { WindowControls } from './TitleBar.tsx';

const windowControls: WindowControls = {
  onMinimise: vi.fn(),
  onToggleMaximise: vi.fn(),
  onClose: vi.fn(),
  isMaximised: false,
};

function renderShell() {
  return render(
    <AppShell currentScreenId="library" onNavigate={vi.fn()} windowControls={windowControls}>
      <p>Screen content</p>
    </AppShell>,
  );
}

describe('AppShell', () => {
  it('heads the content region with the current screen, and names the region after it', () => {
    renderShell();

    expect(screen.getByRole('heading', { level: 1 }).textContent).toBe('Library');
    expect(screen.getByRole('main', { name: 'Library' })).toBeDefined();
    expect(screen.getByText('Screen content')).toBeDefined();
  });

  it('offers a skip link that targets the content region and can take focus', () => {
    renderShell();

    const skipLink = screen.getByRole('link', { name: 'Skip to content' });
    const main = screen.getByRole('main');

    expect(skipLink.getAttribute('href')?.slice(1)).toBe(main.id);
    // Without a tabindex the anchor scrolls the region into view but leaves
    // focus behind, which defeats the point of the link.
    expect(main.getAttribute('tabindex')).toBe('-1');
  });

  it('puts the skip link first, so it is the first thing Tab reaches', () => {
    const { container } = renderShell();

    const focusable = container.querySelectorAll('a[href], button');
    expect(focusable[0]?.textContent).toBe('Skip to content');
  });
});
