// @vitest-environment jsdom
import { render, screen } from '@testing-library/react';
import { afterEach, describe, expect, it } from 'vitest';
import { cleanup } from '@testing-library/react';

import type { Preview, PreviewTrack } from '@clipped/shared';

import { Waveform } from './Waveform';
import { bucketsOver, envelope } from './waveformOutline';

/**
 * What the peaks the recorder answered with turn into (issue #448).
 *
 * The arithmetic is tested directly, because that is the part a component test
 * cannot see: jsdom lays nothing out, so "an `<svg>` is on the screen" is true
 * of a picture of the wrong shape as well as the right one. What the render
 * cases assert is the other half — that the three states of a preview are three
 * different things on the screen, and that none of them is a flat line
 * (`docs/waveforms.md`).
 */

afterEach(cleanup);

/**
 * A track carrying `peaks`, minimum then maximum per bucket.
 *
 * `name` is left out entirely rather than set to `undefined` when there is
 * none, because that is what the wire does — a container that named no track
 * omits the field — and `exactOptionalPropertyTypes` is what makes the two
 * different here.
 */
function track(peaks: number[], name: string | null = 'Game', index = 1): PreviewTrack {
  return {
    index,
    ...(name === null ? {} : { name }),
    sample_rate: 48_000,
    channels: 2,
    duration_seconds: peaks.length / 2 / 100,
    peaks,
  };
}

/** A ready waveform of the tracks given. */
function ready(tracks: PreviewTrack[]): Preview {
  return { kind: 'waveform', state: 'ready', tracks };
}

describe('the outline', () => {
  it('puts full scale at the edges of the lane and silence on its middle', () => {
    // The claim: a peak of ±127 reaches the edge and a peak of zero sits in the
    // middle. A scale that divided by 128, or that forgot the sign, still draws
    // a plausible-looking waveform — and would draw every recording slightly
    // smaller than it is, or upside down.
    const loud = envelope(track([-127, 127]));
    const silent = envelope(track([0, 0]));

    // Lane 40 tall: the maximum is at the top (0) and the minimum at the bottom
    // (40), because SVG's y grows downward and a waveform's minimum is below
    // its middle.
    expect(loud).toBe('M0,0 L0,40 Z');
    expect(silent).toBe('M0,20 L0,20 Z');
  });

  it('runs along the maxima and back along the minima, so one track is one shape', () => {
    // Two buckets, asymmetric on purpose: audio really is asymmetric, and a
    // path that mirrored the maxima instead of using the minima would draw the
    // same shape for both of these.
    const outline = envelope(track([-127, 64, 0, 127]));

    // Forward along the maxima (buckets 0 then 1), then back along the minima
    // (bucket 1 then 0). 64 of 127 is a shade over half, so it is 9.92 above
    // the middle rather than 10 — which is the point of dividing by 127 and not
    // by 128.
    expect(outline).toBe('M0,9.92 L1,0 L1,20 L0,40 Z');
  });

  it('is nothing at all for a track with no peaks in it', () => {
    // Never a flat line: `docs/waveforms.md` is explicit that a line through
    // the middle is indistinguishable from silence, and a track the recorder
    // returned nothing for is not silent, it is unknown.
    expect(envelope(track([]))).toBeNull();
  });
});

describe('the buckets a range of a recording covers', () => {
  /** A track of `buckets` buckets spread over `seconds` of a recording. */
  const over = (buckets: number, seconds: number): PreviewTrack => ({
    ...track(Array.from({ length: buckets * 2 }, () => 0)),
    duration_seconds: seconds,
  });

  it('rounds outwards, so a piece never draws less audio than it covers', () => {
    // One bucket a second. Two and a half to seven and a half seconds is
    // buckets 2 to 8, not 3 to 7: `docs/waveforms.md` rounds peaks outwards for
    // the same reason, which is that somebody hunting for the quiet start of a
    // sound must not be shown less of it than there is.
    expect(bucketsOver(over(60, 60), 2_500_000_000, 7_500_000_000)).toEqual({ from: 2, to: 8 });
  });

  it('is the exact buckets when the range is already on their boundaries', () => {
    expect(bucketsOver(over(60, 60), 30_000_000_000, 38_000_000_000)).toEqual({ from: 30, to: 38 });
  });

  it('clamps to the track rather than reading past its peaks', () => {
    // A segment that reaches past the end of the recording it names — a
    // document written against a file that has since been trimmed. The overlap
    // is drawn and the rest is not invented.
    expect(bucketsOver(over(60, 60), 55_000_000_000, 90_000_000_000)).toEqual({ from: 55, to: 60 });
  });

  it('is nothing at all for a range the track does not reach', () => {
    // Never a flat line, and never the last bucket stretched across the piece.
    expect(bucketsOver(over(60, 60), 90_000_000_000, 95_000_000_000)).toBeNull();
    expect(bucketsOver(over(0, 60), 0, 5_000_000_000)).toBeNull();
    expect(bucketsOver(over(60, 0), 0, 5_000_000_000)).toBeNull();
    expect(bucketsOver(over(60, 60), 8_000_000_000, 8_000_000_000)).toBeNull();
  });

  it('draws only the buckets of the range, re-based so the picture starts at zero', () => {
    // The editor's lanes draw part of a recording (issue #66). The path has to
    // start at x of zero however far into the file the material is, because the
    // `viewBox` it goes in is the slice rather than the track.
    const ramped = track([-1, 1, -2, 2, -3, 3, -4, 4]);

    expect(envelope(ramped, { from: 2, to: 4 })).toBe(envelope(track([-3, 3, -4, 4])));
  });
});

describe('what is drawn', () => {
  it('draws a lane per track, named by what the recording called it', () => {
    render(
      <Waveform
        preview={ready([track([-127, 127], 'Game', 1), track([-10, 10], 'Microphone', 2)])}
        of="cs2.mkv"
      />,
    );

    expect(screen.getByRole('img', { name: 'Sound of Game in cs2.mkv' })).toBeInTheDocument();
    expect(screen.getByRole('img', { name: 'Sound of Microphone in cs2.mkv' })).toBeInTheDocument();
  });

  it('shows a track the recording did not name by its position rather than inventing one', () => {
    render(<Waveform preview={ready([track([-1, 1], null)])} of="clip.mkv" />);

    expect(screen.getByRole('img', { name: 'Sound of Audio 1 in clip.mkv' })).toBeInTheDocument();
  });

  it('says a waveform is still being made rather than drawing an empty lane', () => {
    // The ordinary state of a recording written a moment ago. Drawing an empty
    // lane for it would be indistinguishable from a track that is silent, which
    // is the one confusion `docs/waveforms.md` forbids.
    render(<Waveform preview={{ kind: 'waveform', state: 'pending', tracks: [] }} of="new.mkv" />);

    expect(screen.getByText(/No waveform yet/)).toBeInTheDocument();
    expect(screen.queryAllByRole('img')).toHaveLength(0);
  });

  it('says why there will not be one, and does not say it is coming', () => {
    render(
      <Waveform
        preview={{
          kind: 'waveform',
          state: 'unavailable',
          tracks: [],
          reason: 'that file is in a codec this build has no decoder for',
        }}
        of="odd.mkv"
      />,
    );

    expect(screen.getByText(/no decoder for/)).toBeInTheDocument();
    // The distinction issue #448's second criterion is about, on this screen
    // rather than on a tile: "not yet" and "never" must not read the same.
    expect(screen.queryByText(/yet/)).toBeNull();
  });

  it('says a recording has no sound rather than drawing nothing at all', () => {
    // A successful answer with no tracks in it, which is what every recording
    // Clipped writes today produces (issue #180). Silence about it would leave
    // a blank space somebody has to guess at.
    render(<Waveform preview={ready([])} of="silent.mkv" />);

    expect(screen.getByText(/no sound in it/)).toBeInTheDocument();
  });

  it('draws nothing when nothing has been asked', () => {
    const { container } = render(<Waveform preview={null} of="cs2.mkv" />);

    expect(container).toBeEmptyDOMElement();
  });
});
