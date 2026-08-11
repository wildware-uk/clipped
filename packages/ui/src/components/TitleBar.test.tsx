import { render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { describe, expect, it, vi } from 'vitest';

import { TitleBar, type WindowControls } from './TitleBar.tsx';

function controls(overrides: Partial<WindowControls> = {}): WindowControls {
    return {
        onMinimise: vi.fn(),
        onToggleMaximise: vi.fn(),
        onClose: vi.fn(),
        isMaximised: false,
        ...overrides,
    };
}

describe('TitleBar', () => {
    it('names every window control, because each one is an icon', () => {
        render(<TitleBar name="Clipped" tagline="Game Recorder" windowControls={controls()} />);

        expect(screen.getByRole('button', { name: 'Minimise' })).toBeDefined();
        expect(screen.getByRole('button', { name: 'Maximise' })).toBeDefined();
        expect(screen.getByRole('button', { name: 'Close' })).toBeDefined();
    });

    it('says Restore rather than Maximise once the window is maximised', () => {
        render(
            <TitleBar
                name="Clipped"
                tagline="Game Recorder"
                windowControls={controls({ isMaximised: true })}
            />,
        );

        expect(screen.getByRole('button', { name: 'Restore' })).toBeDefined();
        expect(screen.queryByRole('button', { name: 'Maximise' })).toBeNull();
    });

    it('drives the window from the keyboard', async () => {
        const user = userEvent.setup();
        const windowControls = controls();
        render(
            <TitleBar name="Clipped" tagline="Game Recorder" windowControls={windowControls} />,
        );

        await user.tab();
        await user.keyboard('{Enter}');

        expect(windowControls.onMinimise).toHaveBeenCalledOnce();
    });

    it('keeps the drag region off the buttons, so a click is never swallowed', () => {
        const { container } = render(
            <TitleBar name="Clipped" tagline="Game Recorder" windowControls={controls()} />,
        );

        const dragRegions = container.querySelectorAll('[data-tauri-drag-region]');
        expect(dragRegions).toHaveLength(1);
        expect(dragRegions[0]?.querySelector('button')).toBeNull();
    });
});
