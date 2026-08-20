import { cleanup, render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it, vi } from 'vitest';

import type { Preview } from '@clipped/shared';

import { Waveform } from './Waveform';

/**
 * The waveform, and what issue #694 added to it: a playhead, seeking, and a
 * mark on the track being played.
 *
 * The property worth guarding hardest is the *absence*: this component is drawn
 * for recordings nothing is playing — the Library's rows, a poster with no
 * element behind it — and a playhead over peaks nothing moves through, or a
 * pointer cursor over a picture that does nothing, is the screen lying about
 * what is there (AGENTS.md section 27).
 */

/** Two tracks with peaks, which is what a recording of a game carries. */
const PREVIEW: Preview = {
  kind: 'waveform',
  state: 'ready',
  tracks: [
    {
      index: 0,
      name: 'Game',
      sample_rate: 48_000,
      channels: 2,
      duration_seconds: 100,
      peaks: [0.1, 0.9, 0.4, 0.2],
    },
    {
      index: 1,
      name: 'Microphone',
      sample_rate: 48_000,
      channels: 1,
      duration_seconds: 100,
      peaks: [0.5, 0.2, 0.8, 0.3],
    },
  ],
};

function lanes(): HTMLElement[] {
  return screen.getAllByRole('img');
}

/**
 * A `pointerdown` jsdom will carry.
 *
 * `clientX` is read-only on a constructed event, so it goes in the init rather
 * than being assigned afterwards, and jsdom has no `PointerEvent` — React reads
 * what it needs off a `MouseEvent` of the right type.
 */
function pointerDownAt(clientX: number): MouseEvent {
  const event = new MouseEvent('pointerdown', { bubbles: true, clientX });
  Object.defineProperty(event, 'pointerId', { value: 1 });
  return event;
}

describe('the waveform', () => {
  afterEach(cleanup);

  it('draws one lane per sound track, named', () => {
    render(<Waveform preview={PREVIEW} of="session.mkv" />);

    expect(lanes()).toHaveLength(2);
    expect(screen.getByText('Game')).toBeVisible();
    expect(screen.getByText('Microphone')).toBeVisible();
  });

  it('draws no playhead when nothing is playing it', () => {
    const { container } = render(<Waveform preview={PREVIEW} of="session.mkv" />);

    // The Library's rows and a recording with no element behind it both land
    // here. A playhead would be a mark claiming a position nothing holds.
    expect(container.querySelectorAll('.clipped-waveform__playhead')).toHaveLength(0);
    expect(container.querySelectorAll('.clipped-waveform__lane-picture--seekable')).toHaveLength(0);
  });

  it('places the playhead where the recording has reached', () => {
    const { container } = render(
      <Waveform
        preview={PREVIEW}
        of="session.mkv"
        durationSeconds={100}
        positionSeconds={25}
        onSeek={(): void => {}}
      />,
    );

    const heads = container.querySelectorAll('.clipped-waveform__playhead');
    expect(heads).toHaveLength(2);
    // A quarter of the way through, in the picture's own units. Peaks are
    // stored as min/max pairs, so four of them are *two* buckets
    // (`waveformOutline.bucketCount`) and a quarter across is 0.5. Asserted as
    // a number rather than a string: the point is the arithmetic.
    expect(Number((heads[0] as Element).getAttribute('x'))).toBeCloseTo(0.5);
  });

  it('does not run the playhead off the end when the position is past the length', () => {
    // A container still being written reports a length shorter than what has
    // been played into it, and a playhead outside its own picture is a playhead
    // somewhere the recording is not.
    const { container } = render(
      <Waveform
        preview={PREVIEW}
        of="session.mkv"
        durationSeconds={10}
        positionSeconds={999}
        onSeek={(): void => {}}
      />,
    );

    const head = container.querySelector('.clipped-waveform__playhead');
    // Clamped to the end of the picture: two buckets, so 2 and not 199.8.
    expect(Number(head?.getAttribute('x'))).toBeCloseTo(2);
  });

  it('marks the lane whose track is being played', () => {
    // A media element plays one track at a time. Four identical lanes say
    // nothing about which one is in your ears.
    render(<Waveform preview={PREVIEW} of="session.mkv" durationSeconds={100} playingTrack={1} />);

    expect(screen.getByLabelText(/Microphone.*playing/i)).toBeInTheDocument();
    expect(screen.queryByLabelText(/Game.*playing/i)).toBeNull();
  });

  it('seeks to where the lane was pointed', () => {
    const seeked = vi.fn();
    render(
      <Waveform
        preview={PREVIEW}
        of="session.mkv"
        durationSeconds={200}
        positionSeconds={0}
        onSeek={seeked}
      />,
    );

    const lane = lanes()[0] as Element;
    // jsdom gives every element a zero-sized box, so the width is stated here
    // rather than measured. Without it the component refuses to divide by zero
    // and the test would pass by never calling back.
    lane.getBoundingClientRect = (): DOMRect => ({ left: 100, width: 400 }) as unknown as DOMRect;
    (lane as Element & { setPointerCapture: (id: number) => void }).setPointerCapture =
      (): void => {};

    lane.dispatchEvent(pointerDownAt(200));

    // A quarter across 200 seconds.
    expect(seeked).toHaveBeenCalledTimes(1);
    expect(seeked.mock.calls[0]?.[0]).toBeCloseTo(50);
  });

  it('takes no pointer at all when there is nowhere to seek to', () => {
    const seeked = vi.fn();
    render(<Waveform preview={PREVIEW} of="session.mkv" onSeek={seeked} />);

    const lane = lanes()[0] as Element;
    lane.getBoundingClientRect = (): DOMRect => ({ left: 0, width: 400 }) as unknown as DOMRect;

    // Capturing the pointer is the observable half of "takes no pointer": a
    // lane that grabbed it and then did nothing would swallow a drag that
    // belonged to the page. Asserting only that no seek happened would pass
    // against that, because `seekFrom` refuses without a length as well — the
    // two guards are layered, and this is the one that names the outer.
    let captured = false;
    (lane as Element & { setPointerCapture: (id: number) => void }).setPointerCapture =
      (): void => {
        captured = true;
      };

    lane.dispatchEvent(pointerDownAt(200));

    expect(captured, 'no pointer is captured by a lane that cannot seek').toBe(false);
    expect(seeked).not.toHaveBeenCalled();
    // And it does not offer to, either.
    expect(lane.getAttribute('class')).not.toContain('--seekable');
  });
});
