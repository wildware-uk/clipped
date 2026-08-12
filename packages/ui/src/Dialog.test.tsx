/**
 * The dialog's behaviour, which is the whole reason it is a component.
 *
 * The classes are `components.css`'s and are checked by `stylesheet.test.ts`
 * and `contrast.test.ts`. What is asserted here is what a class cannot carry
 * and what AGENTS.md section 46 asks for: the dialog is announced as one, it is
 * named by its own title, Escape closes it, focus goes into it and comes back,
 * and Tab does not walk out of it into the screen behind.
 */

import { cleanup, render, screen } from '@testing-library/react';
import userEvent from '@testing-library/user-event';
import { useState } from 'react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import { Dialog } from './Dialog';

/** A dialog with two controls in it, opened from a button on the screen. */
function Harness({ onClose = () => undefined }: { readonly onClose?: () => void }) {
  const [open, setOpen] = useState(false);
  return (
    <>
      <button
        type="button"
        onClick={() => {
          setOpen(true);
        }}
      >
        Open
      </button>
      <button type="button">Behind</button>
      {open && (
        <Dialog
          title="Export clip"
          onClose={() => {
            setOpen(false);
            onClose();
          }}
          actions={
            <>
              <button type="button">First</button>
              <button type="button">Last</button>
            </>
          }
        >
          <p>What this would do.</p>
        </Dialog>
      )}
    </>
  );
}

// Vitest runs with `globals: false`, so Testing Library's own automatic
// cleanup - which registers itself against a global `afterEach` - is not
// installed and each case has to unmount what it rendered.
afterEach(cleanup);

describe('Dialog', () => {
  it('is announced as a dialog, named by the title it draws', async () => {
    const user = userEvent.setup();
    render(<Harness />);
    await user.click(screen.getByRole('button', { name: 'Open' }));

    const dialog = screen.getByRole('dialog');
    expect(dialog).toHaveAccessibleName('Export clip');
    expect(dialog).toHaveAttribute('aria-modal', 'true');
    // The name is the heading on screen rather than a second label written for
    // assistive technology, so the two cannot drift apart.
    expect(screen.getByRole('heading', { name: 'Export clip' })).toBeVisible();
  });

  it('takes focus when it opens and gives it back to whatever opened it', async () => {
    const user = userEvent.setup();
    render(<Harness />);
    const opener = screen.getByRole('button', { name: 'Open' });

    await user.click(opener);
    expect(screen.getByRole('dialog')).toHaveFocus();

    await user.keyboard('{Escape}');
    expect(opener).toHaveFocus();
  });

  it('closes on Escape', async () => {
    const onClose = vi.fn();
    const user = userEvent.setup();
    render(<Harness onClose={onClose} />);

    await user.click(screen.getByRole('button', { name: 'Open' }));
    await user.keyboard('{Escape}');

    expect(onClose).toHaveBeenCalledTimes(1);
    expect(screen.queryByRole('dialog')).not.toBeInTheDocument();
  });

  it('cycles the tab order inside itself rather than reaching the screen behind', async () => {
    const user = userEvent.setup();
    render(<Harness />);
    await user.click(screen.getByRole('button', { name: 'Open' }));

    const first = screen.getByRole('button', { name: 'First' });
    const last = screen.getByRole('button', { name: 'Last' });

    await user.tab();
    expect(first).toHaveFocus();
    await user.tab();
    expect(last).toHaveFocus();
    // Past the last control: the screen behind is still in the document's tab
    // order, so this is the assertion that the trap is doing something.
    await user.tab();
    expect(first).toHaveFocus();
    await user.tab({ shift: true });
    expect(last).toHaveFocus();
    expect(screen.getByRole('button', { name: 'Behind' })).not.toHaveFocus();
  });

  it('does not close when the ground around it is clicked', async () => {
    const onClose = vi.fn();
    const user = userEvent.setup();
    const { container } = render(<Harness onClose={onClose} />);
    await user.click(screen.getByRole('button', { name: 'Open' }));

    const scrim = container.querySelector('.clipped-scrim');
    expect(scrim).not.toBeNull();
    await user.click(scrim as Element);

    expect(onClose).not.toHaveBeenCalled();
    expect(screen.getByRole('dialog')).toBeVisible();
  });
});
