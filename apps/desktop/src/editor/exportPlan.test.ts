import { describe, expect, it } from 'vitest';

import { storedDocument } from '../test/editDocumentFixture';
import { readEditDocument, type EditDocument } from './document';
import {
  checksNeedingTheRecording,
  copyOutlook,
  describeBlocker,
  type DocumentBlocker,
} from './exportPlan';

/**
 * The half of the export plan the document settles, held to the cases
 * `crates/export` holds itself to.
 *
 * This file exists for the same reason `timeline.test.ts` does: the answer has
 * to be the same on both sides. `plan.rs`'s own tests assert that a speed, a
 * crop or a rotation is `SegmentTransformed`; that text over the picture is
 * `Overlays { overlays: 1 }`; that a track at the level it was recorded is
 * copied and each of the other four things is a `MixReason`; that a muted
 * track is a mix rather than a missing track, and a solo left on elsewhere
 * never is, because an export is never handed one ([issue
 * #85](https://github.com/wildware-uk/clipped/issues/85)); and that joining two
 * recordings is `SeveralRecordings { recordings: 2 }`. Every one of those is
 * asserted below against the port, with the crate's own shape of document. A
 * test written from the port's own output would prove only that it agrees with
 * itself, and the failure that matters is a dialog telling somebody their clip
 * will be copied and an export then refusing it.
 *
 * The rest of the plan — the keyframe at each cut, the codecs, the picture
 * order, the shape — needs the recording, which this window cannot open
 * (issue #322). Those are not ported, not guessed at, and are named by
 * `checksNeedingTheRecording`; the case at the bottom is what keeps that list
 * from quietly becoming a hedge.
 */

/** One recording, one segment of it, and whatever else the case needs. */
function clip(changes: Record<string, unknown> = {}): EditDocument {
  const read = readEditDocument(
    storedDocument({
      sources: [{ id: 0, recording: 'rec-1' }],
      segments: [
        {
          source: 0,
          span: { start: 1_000_000_000, end: 3_000_000_000 },
          speed: { numerator: 1, denominator: 1 },
          crop: null,
          rotation: 'none',
        },
      ],
      audio_tracks: [],
      overlays: [],
      ...changes,
    }),
  );
  if (!read.ok) {
    throw new Error(`the fixture should read: ${read.problem}`);
  }
  return read.document;
}

/** The blockers of a document that the engine would plan. */
function blockersOf(document: EditDocument): readonly DocumentBlocker[] {
  const outlook = copyOutlook(document);
  if (!outlook.ok) {
    throw new Error(`the document should be plannable: ${outlook.problem}`);
  }
  return outlook.blockers;
}

/** One audio track, spread over whatever the case changes about it. */
function track(changes: Record<string, unknown> = {}): Record<string, unknown> {
  return {
    name: 'Game',
    inputs: [{ source: 0, stream: 0 }],
    gain_db: 0,
    muted: false,
    fade_in: 0,
    fade_out: 0,
    ...changes,
  };
}

describe('what the document settles about copying a clip', () => {
  it('finds nothing against a cut of one recording with no mix and no text', () => {
    // `plan.rs`: `a_cut_on_a_keyframe_with_nothing_else_changed_is_a_copy`, and
    // `an_edit_with_no_mix_carries_the_recordings_audio_as_it_was_recorded`.
    expect(blockersOf(clip())).toEqual([]);
  });

  it('finds nothing against a split into two pieces of the same recording', () => {
    // `plan.rs`: `a_split_into_two_keyframe_aligned_pieces_is_still_a_copy` —
    // what #84's split and delete produce.
    const split = clip({
      segments: [
        {
          source: 0,
          span: { start: 0, end: 2_000_000_000 },
          speed: { numerator: 1, denominator: 1 },
          crop: null,
          rotation: 'none',
        },
        {
          source: 0,
          span: { start: 5_000_000_000, end: 7_000_000_000 },
          speed: { numerator: 1, denominator: 1 },
          crop: null,
          rotation: 'none',
        },
      ],
    });

    expect(blockersOf(split)).toEqual([]);
  });

  it.each([
    ['a speed', { speed: { numerator: 2, denominator: 1 } }, 'sped up'],
    ['a rotation', { rotation: 'clockwise90' }, 'rotated'],
    ['a crop', { crop: { x: 0, y: 0, width: 0.5, height: 1 } }, 'cropped'],
  ])('reports %s as a transformed segment, and says which', (_name, change, expected) => {
    // `plan.rs`: `a_speed_a_crop_or_a_rotation_is_a_re_encode`, which reports
    // `SegmentTransformed { segment: 0 }` for all three. The crate has no word
    // for which of the three it was; the dialog has the document, so it does.
    const document = clip({
      segments: [
        {
          source: 0,
          span: { start: 0, end: 2_000_000_000 },
          speed: { numerator: 1, denominator: 1 },
          crop: null,
          rotation: 'none',
          ...change,
        },
      ],
    });

    const blockers = blockersOf(document);
    expect(blockers).toEqual([{ kind: 'segmentTransformed', segment: 0 }]);
    expect(describeBlocker(document, blockers[0] as DocumentBlocker).what).toBe(
      `Segment 1 is ${expected}.`,
    );
  });

  it('does not report a crop that takes the whole frame', () => {
    // `CropRect::FULL` is not a transformation: `crates/edit`'s
    // `a_full_frame_crop_is_not_a_transformation`.
    const document = clip({
      segments: [
        {
          source: 0,
          span: { start: 0, end: 2_000_000_000 },
          speed: { numerator: 1, denominator: 1 },
          crop: { x: 0, y: 0, width: 1, height: 1 },
          rotation: 'none',
        },
      ],
    });

    expect(blockersOf(document)).toEqual([]);
  });

  it('reports text over the picture once, however many pieces there are', () => {
    // `plan.rs`: `text_over_the_picture_is_a_re_encode`.
    const one = {
      text: 'Ace',
      when: { start: 0, end: 1 },
      position: { x: 0, y: 0 },
      height_percent: 7,
    };
    const document = clip({ overlays: [one, { ...one, text: 'Round 12' }] });

    const blockers = blockersOf(document);
    expect(blockers).toEqual([{ kind: 'overlays', overlays: 2 }]);
    expect(describeBlocker(document, blockers[0] as DocumentBlocker).what).toBe(
      '2 pieces of text are drawn over the picture.',
    );
  });

  it('reports nothing against a track played at the level it was recorded', () => {
    // `plan.rs`: the first half of
    // `a_track_at_the_level_it_was_recorded_is_copied_and_anything_else_is_mixed`.
    expect(blockersOf(clip({ audio_tracks: [track()] }))).toEqual([]);
  });

  it.each([
    [
      'a level',
      { gain_db: -6 },
      'level',
      'The “Game” track plays at -6.0 dB rather than the level it was recorded at.',
    ],
    ['a mute', { muted: true }, 'silenced', 'The “Game” track is muted.'],
    ['a fade', { fade_in: 500_000_000 }, 'fades', 'The “Game” track fades in over 0.5 s.'],
    [
      'two inputs',
      {
        inputs: [
          { source: 0, stream: 0 },
          { source: 0, stream: 1 },
        ],
      },
      'severalInputs',
      'The “Game” track sums 2 recorded streams.',
    ],
  ])('reports %s as a mix, and says what makes it one', (_name, change, reason, sentence) => {
    // `plan.rs`: the four cases of
    // `a_track_at_the_level_it_was_recorded_is_copied_and_anything_else_is_mixed`,
    // in the same four `MixReason`s.
    const document = clip({ audio_tracks: [track(change)] });

    const blockers = blockersOf(document);
    expect(blockers).toEqual([{ kind: 'trackNeedsMixing', track: 0, reason }]);
    expect(describeBlocker(document, blockers[0] as DocumentBlocker).what).toBe(sentence);
  });

  it('is unmoved by which track the editor happens to be soloing, because soloing is not saved', () => {
    // `plan.rs`: `a_silenced_track_is_a_mix_rather_than_a_missing_track`, whose
    // own comment says it: "muting is the only way a track is silent in an
    // export; soloing moved out of the document in #85, so an export is never
    // handed one." A document has nowhere left to carry a solo at all — this
    // is the proof that the port does not go looking for one anyway.
    const document = clip({
      audio_tracks: [track(), track({ name: 'Microphone', muted: true })],
    });

    const blockers = blockersOf(document);
    expect(blockers).toEqual([{ kind: 'trackNeedsMixing', track: 1, reason: 'silenced' }]);
    expect(describeBlocker(document, blockers[0] as DocumentBlocker).what).toBe(
      'The “Microphone” track is muted.',
    );
  });

  it('reports a mute ahead of the level and the fades on the same track', () => {
    // `plan_audio`'s order: silence is what comes out of a track that is muted
    // *and* boosted, so that is what is said about it.
    const document = clip({
      audio_tracks: [track({ muted: true, gain_db: -6, fade_in: 500_000_000 })],
    });

    expect(blockersOf(document)).toEqual([
      { kind: 'trackNeedsMixing', track: 0, reason: 'silenced' },
    ]);
  });

  it('reports two recordings as one reason, and does not go on to the segments', () => {
    // `plan.rs`: `joining_two_recordings_is_a_re_encode` reports
    // `SeveralRecordings` alone. `check_segments` needs the one recording's
    // profile and does not run without it, so the sped-up segment below is
    // deliberately *not* reported: this list has to be the engine's list.
    const document = clip({
      sources: [
        { id: 0, recording: 'rec-1' },
        { id: 1, recording: 'rec-2' },
      ],
      segments: [
        {
          source: 0,
          span: { start: 0, end: 1_000_000_000 },
          speed: { numerator: 2, denominator: 1 },
          crop: null,
          rotation: 'none',
        },
        {
          source: 1,
          span: { start: 0, end: 1_000_000_000 },
          speed: { numerator: 1, denominator: 1 },
          crop: null,
          rotation: 'none',
        },
      ],
    });

    const blockers = blockersOf(document);
    expect(blockers).toEqual([{ kind: 'severalRecordings', recordings: 2 }]);
    expect(describeBlocker(document, blockers[0] as DocumentBlocker).what).toBe(
      'This clip joins 2 recordings.',
    );
  });

  it('says a clip with no material has none, rather than that it joins none', () => {
    const document = clip({ segments: [] });

    const blockers = blockersOf(document);
    expect(blockers).toEqual([{ kind: 'severalRecordings', recordings: 0 }]);
    expect(describeBlocker(document, blockers[0] as DocumentBlocker).what).toBe(
      'This clip has no material: no segment of it names a recording it declares.',
    );
  });

  it('lists the reasons in the order the engine collects them', () => {
    // `ExportPlan::of` pushes the recordings, then the audio tracks, then the
    // overlays, then the segments. Somebody comparing this list with an
    // export's own refusal should not have to reconcile two orders.
    const document = clip({
      audio_tracks: [track({ gain_db: -6 })],
      overlays: [
        { text: 'Ace', when: { start: 0, end: 1 }, position: { x: 0, y: 0 }, height_percent: 7 },
      ],
      segments: [
        {
          source: 0,
          span: { start: 0, end: 1_000_000_000 },
          speed: { numerator: 2, denominator: 1 },
          crop: null,
          rotation: 'none',
        },
      ],
    });

    expect(blockersOf(document).map((blocker) => blocker.kind)).toEqual([
      'trackNeedsMixing',
      'overlays',
      'segmentTransformed',
    ]);
  });

  it('refuses to answer for a document the engine would refuse to plan', () => {
    // `EditDocument::validate` refuses a track with no inputs and
    // `ExportPlan::of` runs it first, so the engine's answer is a failure
    // rather than a plan. "Nothing rules out a copy" would be worse than
    // saying so.
    const outlook = copyOutlook(clip({ audio_tracks: [track({ inputs: [] })] }));

    expect(outlook.ok).toBe(false);
    expect(outlook.ok ? '' : outlook.problem).toContain('“Game”');
  });

  it('reads every reason out of the editor’s own fixture clip', () => {
    // The three-segment clip in `docs/editing.md`: two recordings, a Game track
    // summing both, a muted Microphone, and one overlay. Four reasons, in the
    // engine's order, from a document nothing in this file built.
    const read = readEditDocument(storedDocument());
    if (!read.ok) {
      throw new Error(read.problem);
    }

    expect(blockersOf(read.document)).toEqual([
      { kind: 'severalRecordings', recordings: 2 },
      { kind: 'trackNeedsMixing', track: 0, reason: 'severalInputs' },
      { kind: 'trackNeedsMixing', track: 1, reason: 'silenced' },
      { kind: 'overlays', overlays: 1 },
    ]);
  });
});

describe('what deciding a copy still needs the recording for', () => {
  it('names the checks that need the file, and the shape only when one is asked for', () => {
    // One entry per check `ExportPlan::of` makes against a `SourceProfile`. The
    // shape is the conditional one: `check_video` compares it only when the
    // document carries an aspect ratio.
    const checks = checksNeedingTheRecording(clip({ aspect_ratio: null }));

    expect(checks).toHaveLength(4);
    expect(checks.join(' ')).toContain('keyframe');
    expect(checks.join(' ')).not.toContain('shape');

    const shaped = checksNeedingTheRecording(clip({ aspect_ratio: { width: 9, height: 16 } }));
    expect(shaped).toHaveLength(5);
    expect(shaped[4]).toContain('9:16');
  });
});
