/**
 * An edit document as text, for the tests that read or draw one.
 *
 * The figures are `crates/edit`'s own: the three-segment clip in
 * `timeline.rs`'s documentation and in `docs/editing.md` — eight seconds of one
 * recording, twelve more from further into it, then four seconds of a second
 * recording — with the audio tracks and the overlay the document example there
 * carries. Using the crate's numbers is the point: `timeline.test.ts` asserts
 * the same answers `crates/edit`'s tests assert, so a port of the arithmetic
 * that disagreed with the crate would fail here rather than in an export
 * somebody made six months from now.
 *
 * It is written as **text**, because that is what a stored document is and what
 * the screen is given: a fixture built as an object would skip the reader,
 * which is half of what these tests are about.
 */

/** The three-segment clip, as the object it serialises from. */
function base(): Record<string, unknown> {
  return {
    schema_version: 1,
    title: 'Round 12 ace',
    aspect_ratio: { width: 16, height: 9 },
    sources: [
      { id: 0, recording: 'rec-2026-08-11-cs2' },
      { id: 1, recording: 'rec-2026-08-11-cs2-b' },
    ],
    segments: [
      {
        source: 0,
        span: { start: 30_000_000_000, end: 38_000_000_000 },
        speed: { numerator: 1, denominator: 1 },
        crop: null,
        rotation: 'none',
      },
      {
        source: 0,
        span: { start: 92_000_000_000, end: 104_000_000_000 },
        speed: { numerator: 1, denominator: 1 },
        crop: null,
        rotation: 'none',
      },
      {
        source: 1,
        span: { start: 5_000_000_000, end: 9_000_000_000 },
        speed: { numerator: 1, denominator: 1 },
        crop: null,
        rotation: 'none',
      },
    ],
    audio_tracks: [
      {
        name: 'Game',
        inputs: [
          { source: 0, stream: 0 },
          { source: 1, stream: 0 },
        ],
        gain_db: -3,
        muted: false,
        soloed: false,
        fade_in: 0,
        fade_out: 1_000_000_000,
      },
      {
        name: 'Microphone',
        inputs: [{ source: 0, stream: 1 }],
        gain_db: 0,
        muted: true,
        soloed: false,
        fade_in: 0,
        fade_out: 0,
      },
    ],
    overlays: [
      {
        text: 'Round 12',
        when: { start: 0, end: 3_000_000_000 },
        position: { x: 0.5, y: 0.85 },
        height_percent: 7,
      },
    ],
  };
}

/** The fixture, with `changes` applied to its top level, as stored text. */
export function storedDocument(changes: Record<string, unknown> = {}): string {
  return JSON.stringify({ ...base(), ...changes });
}

/** How long the fixture's clip lasts: 8s + 12s + 4s. */
export const FIXTURE_NANOS = 24_000_000_000;
