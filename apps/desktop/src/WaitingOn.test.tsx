import { cleanup, render, screen, within } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';

import { WaitingOn } from './WaitingOn';

/**
 * The table three screens use to say what they cannot show yet.
 *
 * The case that matters is the empty one, and it is new: the Games screen
 * finished its list when #245 landed, and a component that drew two column
 * headings over no rows would have left a table reading as one that failed to
 * load. That is the opposite of what an empty list means.
 */
describe('the waiting-on table', () => {
  afterEach(cleanup);

  it('draws nothing at all when there is nothing left to wait for', () => {
    render(<WaitingOn heading="What this screen will show" rows={[]} />);

    // Not "an empty table" — no table, and no heading either. A reader who sees
    // the headings believes the list is coming and it never was.
    expect(screen.queryByRole('table')).toBeNull();
    expect(screen.queryByText(/what this screen will show/i)).toBeNull();
  });

  it('names each row and what has to exist first', () => {
    render(
      <WaitingOn
        heading="What this screen will show"
        rows={[{ shows: 'A thing', needs: 'Another thing. Issue #1' }]}
      />,
    );

    const table = screen.getByRole('table', { name: 'What this screen will show' });
    const cells = within(within(table).getAllByRole('row')[1] as HTMLElement).getAllByRole('cell');
    expect(cells[0]).toHaveTextContent('A thing');
    expect(cells[1]).toHaveTextContent('Issue #1');
  });
});
