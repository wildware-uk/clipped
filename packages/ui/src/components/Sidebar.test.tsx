import { SCREENS } from '@clipped/shared';
import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import { Sidebar } from './Sidebar.tsx';

describe('Sidebar', () => {
    it('offers every screen', () => {
        render(<Sidebar currentScreenId="home" onNavigate={vi.fn()} />);

        const items = screen.getAllByRole('button');
        expect(items.map((item) => item.textContent)).toEqual(SCREENS.map((s) => s.label));
    });

    it('marks the current screen for assistive technology, not only in colour', () => {
        render(<Sidebar currentScreenId="games" onNavigate={vi.fn()} />);

        expect(screen.getByRole('button', { current: 'page' }).textContent).toBe('Games');
    });

    it('reaches every screen with the keyboard alone, and activates with Enter', async () => {
        const user = userEvent.setup();
        const onNavigate = vi.fn();
        render(<Sidebar currentScreenId="home" onNavigate={onNavigate} />);

        // Tab from the top of the document to the third entry, then press
        // Enter. This is the whole of AGENTS.md section 46's "keyboard
        // navigation" claim for the sidebar: no pointer is involved.
        await user.tab();
        await user.tab();
        await user.tab();
        await user.keyboard('{Enter}');

        expect(document.activeElement?.textContent).toBe(SCREENS[2]?.label);
        expect(onNavigate).toHaveBeenCalledExactlyOnceWith(SCREENS[2]?.id);
    });

    it('renders a footer only when it is given one', () => {
        const { rerender, container } = render(
            <Sidebar currentScreenId="home" onNavigate={vi.fn()} />,
        );
        expect(container.querySelector('.app-sidebar-footer')).toBeNull();

        rerender(<Sidebar currentScreenId="home" onNavigate={vi.fn()} footer={<p>Version 0</p>} />);
        expect(screen.getByText('Version 0')).toBeDefined();
    });
});
