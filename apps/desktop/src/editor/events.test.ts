// @vitest-environment node
//
// What the timeline knows about game events, and the one thing it must never
// do: skip one (issue #71, AGENTS.md section 27).
//
// The first case reads `crates/events/src/kind.rs` and compares its vocabulary
// against the labels here. A hand-written list of fourteen strings is exactly
// the kind that goes stale the day somebody adds a fifteenth, and the failure
// it produces is silent — a mark drawn with a raw tag for a kind the
// application does in fact act on. It runs in the node environment for the
// reason `stylesheet.test.ts` does: the subject is a file on disk.

import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

import { describe, expect, it } from 'vitest';

import { storedDocument } from '../test/editDocumentFixture';
import { readEditDocument, type EditDocument } from './document';
import { countByKind, describeKind, kindsPresent, KNOWN_KINDS, type EventMark } from '../events';
import { outputPositionsOf } from './timeline';

/** The two recordings the fixture clip draws on. */
const FIRST = 'rec-2026-08-11-cs2';
const SECOND = 'rec-2026-08-11-cs2-b';

const SECOND_NANOS = 1_000_000_000;

function mark(kind: string, atSeconds: number, recording = FIRST): EventMark {
  return { recording, at: atSeconds * SECOND_NANOS, kind, source: 'counter-strike-2' };
}

/** The fixture clip, read as the screen reads it. */
function clip(): EditDocument {
  const read = readEditDocument(storedDocument());
  if (!read.ok) {
    throw new Error(read.problem);
  }
  return read.document;
}

describe('the event vocabulary', () => {
  /**
   * Every tag `EventKind::as_str` returns for a variant of the closed
   * vocabulary, read out of the crate.
   *
   * `Custom` and `Unrecognised` are deliberately excluded: both return a string
   * they were handed rather than a literal, which is what makes them the open
   * half, and the arms are matched by shape rather than by name so that the two
   * cannot be mistaken for members of the list.
   */
  function crateKinds(): readonly string[] {
    const path = fileURLToPath(new URL('../../../../crates/events/src/kind.rs', import.meta.url));
    const source = readFileSync(path, 'utf8');
    const body = /pub fn as_str\(&self\) -> &str \{([\s\S]*?)\n {4}\}/.exec(source);
    if (body === null) {
      throw new Error('the `as_str` match could not be found in crates/events/src/kind.rs');
    }
    return [...(body[1] ?? '').matchAll(/Self::\w+ => "([a-z_]+)",/g)].map(
      (found) => found[1] ?? '',
    );
  }

  it('is exactly the one crates/events declares, in the order it declares it', () => {
    const fromCrate = crateKinds();

    expect(fromCrate.length).toBeGreaterThan(0);
    expect(KNOWN_KINDS).toEqual(fromCrate);
  });

  it('gives every kind it knows a name that is not the tag', () => {
    for (const kind of KNOWN_KINDS) {
      const description = describeKind(kind);
      expect(description.known).toBe(true);
      expect(description.label).not.toBe(kind);
      expect(description.plugin).toBeNull();
    }
  });

  it('names a kind this build has never met with the tag it arrived with', () => {
    // A kind added to the vocabulary after this build shipped. It is not
    // discarded and it is not renamed into something nobody wrote: the mark is
    // drawn, and the label says the word the recorder stored.
    const description = describeKind('objective_taken');

    expect(description.label).toBe('objective_taken');
    expect(description.known).toBe(false);
    expect(description.plugin).toBeNull();
  });

  it('attributes a plugin’s own word to the plugin that invented it', () => {
    const description = describeKind('acme-cs2.flag_captured');

    expect(description.label).toBe('acme-cs2.flag_captured');
    expect(description.known).toBe(false);
    expect(description.plugin).toBe('acme-cs2');
  });

  it('lists the kinds present with the known ones first and the rest in order', () => {
    // Fixed rather than "as encountered", so the filter does not rearrange
    // itself between two sessions of the same game.
    const marks = [
      mark('acme-cs2.flag_captured', 30),
      mark('death', 31),
      mark('objective_taken', 32),
      mark('kill', 33),
      mark('kill', 34),
    ];

    expect(kindsPresent(marks)).toEqual([
      'kill',
      'death',
      'acme-cs2.flag_captured',
      'objective_taken',
    ]);
    expect(countByKind(marks).get('kill')).toBe(2);
    expect(countByKind(marks).get('objective_taken')).toBe(1);
  });

  it('has no kinds and no counts when nothing happened', () => {
    expect(kindsPresent([])).toEqual([]);
    expect(countByKind([]).size).toBe(0);
  });
});

/*
 * Where an event of a recording lands on an *edited* timeline. The fixture is
 * `crates/edit`'s own three-segment clip: 30-38s of the first recording, then
 * 92-104s of the same one, then 5-9s of a second.
 */
describe('placing an event on the edited timeline', () => {
  it('puts a moment of the first segment at its offset into the clip', () => {
    expect(outputPositionsOf(clip(), FIRST, 34 * SECOND_NANOS)).toEqual([4 * SECOND_NANOS]);
  });

  it('rebases a moment of a later segment onto where that segment starts', () => {
    // 95s of the recording is three seconds into a segment that begins eight
    // seconds into the clip. A subtraction from the recording's own zero would
    // put it at 95 seconds of a 24-second clip.
    expect(outputPositionsOf(clip(), FIRST, 95 * SECOND_NANOS)).toEqual([11 * SECOND_NANOS]);
  });

  it('tells the two recordings apart', () => {
    expect(outputPositionsOf(clip(), SECOND, 6 * SECOND_NANOS)).toEqual([21 * SECOND_NANOS]);
    expect(
      outputPositionsOf(clip(), FIRST, 6 * SECOND_NANOS),
      'the second recording’s seconds must not be drawn on the first',
    ).toEqual([]);
  });

  it('draws nothing for a moment the clip trimmed out', () => {
    // 50s of the recording is in neither of the parts this clip uses. A marker
    // at the nearest cut would claim footage the clip does not contain.
    expect(outputPositionsOf(clip(), FIRST, 50 * SECOND_NANOS)).toEqual([]);
  });

  it('excludes the end of a segment, which belongs to the next one', () => {
    // Half-open, like every other range in the editor: 38s is where the first
    // segment stops, so it is not in it.
    expect(outputPositionsOf(clip(), FIRST, 38 * SECOND_NANOS)).toEqual([]);
    expect(outputPositionsOf(clip(), FIRST, 30 * SECOND_NANOS)).toEqual([0]);
  });

  it('draws a moment twice when the clip uses those seconds twice', () => {
    // Not a hypothetical: a clip that shows the same play again is one segment
    // repeated, and one of the two marks would be a mark the footage has.
    const twice = readEditDocument(
      storedDocument({
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
            span: { start: 30_000_000_000, end: 38_000_000_000 },
            speed: { numerator: 1, denominator: 1 },
            crop: null,
            rotation: 'none',
          },
        ],
      }),
    );
    if (!twice.ok) {
      throw new Error(twice.problem);
    }

    expect(outputPositionsOf(twice.document, FIRST, 34 * SECOND_NANOS)).toEqual([
      4 * SECOND_NANOS,
      12 * SECOND_NANOS,
    ]);
  });

  it('follows a segment’s speed, so a slowed moment is where it is played', () => {
    // Half speed: a moment four seconds into the material is eight seconds into
    // the clip. Getting this wrong is a mark that drifts further from its
    // footage the longer the segment runs.
    const slowed = readEditDocument(
      storedDocument({
        segments: [
          {
            source: 0,
            span: { start: 30_000_000_000, end: 38_000_000_000 },
            speed: { numerator: 1, denominator: 2 },
            crop: null,
            rotation: 'none',
          },
        ],
      }),
    );
    if (!slowed.ok) {
      throw new Error(slowed.problem);
    }

    expect(outputPositionsOf(slowed.document, FIRST, 34 * SECOND_NANOS)).toEqual([
      8 * SECOND_NANOS,
    ]);
  });
});
