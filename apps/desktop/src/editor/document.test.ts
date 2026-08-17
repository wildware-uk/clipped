import { describe, expect, it } from 'vitest';

import { storedDocument } from '../test/editDocumentFixture';
import { readEditDocument, recordingOf } from './document';

/**
 * Reading a stored edit document, and refusing one.
 *
 * The refusals are the point. `docs/editing.md`'s compatibility table is a
 * contract about user data that has to survive updates — including the update
 * the user has not installed on their other machine — and every row of it says
 * the same thing: refuse and change nothing, rather than open and quietly
 * discard. A reader that opened a document it did not understand would put the
 * editor in front of a clip that is not the one the user made.
 */

/** The document read out of `text`, or a failure naming why it would not read. */
function read(text: string) {
  const found = readEditDocument(text);
  if (!found.ok) {
    throw new Error(found.problem);
  }
  return found.document;
}

/** The fixture as a tree these tests can break in one place. */
function mutableFixture(): Record<string, unknown> {
  return JSON.parse(storedDocument()) as Record<string, unknown>;
}

/**
 * The object at a dotted path of `document`, or a failure naming the step that
 * is not there — so a fixture that changes shape fails rather than quietly
 * putting an unknown field somewhere nobody meant.
 */
function dig(document: Record<string, unknown>, path: string): Record<string, unknown> {
  let found: unknown = document;
  for (const step of path.split('.').filter((part) => part !== '')) {
    if (typeof found !== 'object' || found === null) {
      throw new Error(`the fixture has no "${path}"`);
    }
    found = (found as Record<string, unknown>)[step];
  }
  if (typeof found !== 'object' || found === null) {
    throw new Error(`the fixture has no "${path}"`);
  }
  return found as Record<string, unknown>;
}

/** Why `text` was refused, or a failure saying it was not. */
function refusal(text: string): string {
  const found = readEditDocument(text);
  if (found.ok) {
    throw new Error('this document was read when it should have been refused');
  }
  return found.problem;
}

describe('a stored edit document', () => {
  it('reads back everything the model writes', () => {
    const document = read(storedDocument());

    expect(document.title).toBe('Round 12 ace');
    expect(document.aspect_ratio).toEqual({ width: 16, height: 9 });
    expect(document.sources).toHaveLength(2);
    expect(document.segments).toHaveLength(3);
    expect(document.segments[0]).toEqual({
      source: 0,
      span: { start: 30_000_000_000, end: 38_000_000_000 },
      speed: { numerator: 1, denominator: 1 },
      crop: null,
      rotation: 'none',
    });
    expect(document.audio_tracks[1]).toMatchObject({ name: 'Microphone', muted: true });
    expect(document.overlays[0]).toEqual({
      text: 'Round 12',
      when: { start: 0, end: 3_000_000_000 },
      position: { x: 0.5, y: 0.85 },
      height_percent: 7,
    });
  });

  /*
   * Every one of these fields carries `#[serde(default)]` in the model, so a
   * document written without them is one `crates/edit` produced rather than a
   * broken one, and the defaults have to be the same defaults.
   */
  it('takes the model’s own defaults for what a document may leave out', () => {
    const document = read(
      JSON.stringify({
        schema_version: 2,
        title: 'Bare',
        sources: [{ id: 0, recording: 'rec-1' }],
        segments: [{ source: 0, span: { start: 0, end: 1_000_000_000 } }],
      }),
    );

    expect(document.aspect_ratio).toBeNull();
    expect(document.audio_tracks).toEqual([]);
    expect(document.overlays).toEqual([]);
    expect(document.segments[0]).toMatchObject({
      speed: { numerator: 1, denominator: 1 },
      crop: null,
      rotation: 'none',
    });
  });

  it('reads the empty clip, which a user who deleted everything has', () => {
    const document = read(
      JSON.stringify({ schema_version: 2, title: 'Empty', sources: [], segments: [] }),
    );

    expect(document.segments).toEqual([]);
  });

  it('names the recording a segment plays, and says nothing when the source is undeclared', () => {
    const document = read(storedDocument());

    expect(recordingOf(document, 0)).toBe('rec-2026-08-11-cs2');
    expect(recordingOf(document, 1)).toBe('rec-2026-08-11-cs2-b');
    expect(recordingOf(document, 7)).toBeUndefined();
  });
});

describe('a document that will not load', () => {
  it('says so when it is not JSON at all', () => {
    expect(refusal('{')).toMatch(/not valid JSON/);
  });

  it('says so when it is JSON but not an object', () => {
    expect(refusal('[1, 2]')).toMatch(/should be an object/);
  });

  it('refuses a document that does not say which format it is in', () => {
    expect(refusal(JSON.stringify({ title: 'No version', sources: [], segments: [] }))).toMatch(
      /does not say which format/,
    );
  });

  /*
   * The row of the compatibility table that matters most: a document from a
   * build that knows more than this one. Refusing by *version* is why the
   * version is read out of the raw JSON before anything else — a version-3
   * document may have a shape this build cannot deserialise at all, and it
   * still has to be refused as a version rather than as a parse failure.
   */
  it('refuses a document from a newer Clipped, and says nothing has been changed', () => {
    const problem = refusal(
      JSON.stringify({ schema_version: 3, tone_map: 'hlg', anything: [1, 2, 3] }),
    );

    expect(problem).toMatch(/format 3/);
    expect(problem).toMatch(/Update Clipped/);
    expect(problem).toMatch(/Nothing has been changed/);
  });

  it('refuses a document older than this build rather than converting one it cannot store', () => {
    expect(refusal(JSON.stringify({ schema_version: 0, sources: [], segments: [] }))).toMatch(
      /cannot convert/,
    );
  });

  /*
   * The real case rather than a hypothetical one: format 1 is what every
   * document on disk before this build was, and `crates/edit` still opens it —
   * converting `soloed` away in memory (`SOLO_IS_NOT_AN_EDIT`,
   * `crates/edit/src/schema.rs`) and telling its caller it did. This window is
   * not that caller: it cannot store the result, so it refuses rather than
   * silently opening a document with `soloed` dropped out from under it.
   */
  it('refuses a genuine format 1 document, carrying soloed, rather than converting it', () => {
    const versionOne = JSON.parse(storedDocument()) as Record<string, unknown>;
    versionOne.schema_version = 1;
    const tracks = versionOne.audio_tracks as Record<string, unknown>[];
    for (const track of tracks) {
      track.soloed = false;
    }

    expect(refusal(JSON.stringify(versionOne))).toMatch(/cannot convert/);
  });

  /*
   * `deny_unknown_fields` is on every structure the model reads, and
   * `docs/editing.md` argues why: an older build that opened a newer document,
   * dropped the field it did not understand and wrote that back would lose
   * whatever the user had set. The sweep is over every object of a fully
   * populated document, so a structure added to the model later is covered
   * without anybody adding it to a list.
   */
  const PATHS: readonly string[] = [
    '',
    'aspect_ratio',
    'sources.0',
    'segments.0',
    'segments.0.span',
    'segments.0.speed',
    'audio_tracks.0',
    'audio_tracks.0.inputs.0',
    'overlays.0',
    'overlays.0.when',
    'overlays.0.position',
  ];

  it.each(PATHS)('refuses a field this build does not understand, in "%s"', (path) => {
    const document = mutableFixture();
    dig(document, path).something_new = 'from a later Clipped';

    expect(refusal(JSON.stringify(document))).toMatch(/does not understand/);
  });

  it('names the field whose value is the wrong shape', () => {
    const document = mutableFixture();
    dig(document, 'segments.0.span').start = '30s';

    expect(refusal(JSON.stringify(document))).toBe(
      'segments[0].span.start should be a whole number of nanoseconds.',
    );
  });

  it('refuses a document with no segments field at all, which the model has no default for', () => {
    expect(refusal(JSON.stringify({ schema_version: 2, title: 'x', sources: [] }))).toMatch(
      /no "segments"/,
    );
  });

  it('refuses a rotation this build has no name for', () => {
    const document = mutableFixture();
    dig(document, 'segments.0').rotation = 'clockwise45';

    expect(refusal(JSON.stringify(document))).toMatch(/segments\[0\].rotation should be one of/);
  });
});
