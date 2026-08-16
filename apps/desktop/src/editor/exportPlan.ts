/**
 * What exporting this clip would do, as far as the document itself settles it.
 *
 * # What the engine does, and why only half of it is here
 *
 * `crates/export` decides between two ways of making a file
 * (`docs/exporting.md`). A **stream copy** writes the recording's own coded
 * packets: it is about as fast as reading the file, and the pictures come out
 * bit for bit. A **re-encode** decodes, transforms and encodes every frame —
 * and it is **not built**. So a plan that is not a copy means the export is
 * refused today, with every reason named (`ExportError::ReencodeRequired`),
 * rather than made slowly.
 *
 * `ExportPlan::of` answers that from two things: the edit document, and one
 * demuxing pass over the recording. This window has the document — it is the
 * text the editor already reads — and cannot open the recording at all
 * ([#322](https://github.com/wildware-uk/clipped/issues/322)). So this file
 * ports exactly the half the document settles, and
 * {@link checksNeedingTheRecording} names the half it cannot answer rather than
 * guessing at it (AGENTS.md section 27).
 *
 * That split is not an approximation of the crate's logic; it is the crate's
 * own structure. `ExportPlan::of` collects `SeveralRecordings`, the audio mix
 * reasons and `Overlays` from the document, and `check_video` and
 * `check_segments`, which need a `SourceProfile`, from the file.
 *
 * # Why the checks are written out here rather than asked for
 *
 * The same reason `timeline.ts` exists: the window links exactly one crate of
 * the workspace and that one is the control protocol
 * (`tests/integration/tests/workspace_layering.rs`), so `clipped-export` cannot
 * be called from here. `ExportPlan`, `CopyBlocker` and `MixReason` do not
 * derive `serde` either, so there is not even a wire form to render — #322
 * carries that too. What is below follows `crates/export/src/plan.rs` variant
 * for variant, in the order that file pushes them, so the two can be read side
 * by side.
 *
 * # Why the sentences are not the crate's
 *
 * `CopyBlocker`'s `Display` writes for a log: "the recording's picture is
 * {codec}, which Clipped's container writer cannot describe". A dialog has to
 * say which edit is responsible and what to change about it (AGENTS.md
 * sections 28 and 45), and it has the document open, so it can say *which*
 * transformation, *what* level and *how many* streams where the crate says
 * "transformed" and "a mix". {@link describeBlocker} is that, and it is the
 * only place in the window that words a blocker.
 *
 * # Nothing here writes anything
 *
 * Reading a document to say what an export would be does not touch a
 * recording, and cannot: this window has no file-system permission at all
 * (AGENTS.md sections 56 and 57).
 */

import {
  recordingOf,
  type AudioTrack,
  type CropRect,
  type EditDocument,
  type Segment,
} from './document';
import { resolve, NANOS_PER_SECOND } from './timeline';

/**
 * Why an audio track cannot be copied as it stands: `MixReason` in
 * `crates/export/src/plan.rs`.
 */
export type MixReason = 'severalInputs' | 'level' | 'silenced' | 'fades';

/**
 * One reason this clip cannot be exported by copying its packets, that the
 * document alone settles.
 *
 * Each is a variant of the crate's `CopyBlocker`. The variants that need the
 * recording — `NoVideo`, `VideoCodecNotDescribable`,
 * `AudioCodecNotDescribable`, `SegmentHasNoFrames`,
 * `SegmentDoesNotStartOnAKeyframe`, `ReorderedStream` and `AspectRatioDiffers`
 * — are deliberately absent rather than guessed at; see
 * {@link checksNeedingTheRecording}.
 *
 * A track is carried by its position rather than by the name the crate's
 * variant carries, because the dialog says what the track's level and inputs
 * are and the document is where those live.
 */
export type DocumentBlocker =
  | { readonly kind: 'severalRecordings'; readonly recordings: number }
  | { readonly kind: 'trackNeedsMixing'; readonly track: number; readonly reason: MixReason }
  | { readonly kind: 'overlays'; readonly overlays: number }
  | { readonly kind: 'segmentTransformed'; readonly segment: number };

/**
 * What the document says about copying this clip.
 *
 * `problem` is the case where the engine would not produce a plan at all:
 * `ExportPlan::of` validates the document before anything else and fails rather
 * than reporting blockers. "Nothing rules out a copy", said about a document the
 * engine would refuse to plan, is the more confident of the two wrong answers.
 */
export type CopyOutlook =
  | { readonly ok: false; readonly problem: string }
  | { readonly ok: true; readonly blockers: readonly DocumentBlocker[] };

/** A blocker in words: what is responsible, and what to change. */
export interface Reason {
  /** Which edit rules out a copy, named specifically. */
  readonly what: string;
  /** What changing it would do, or why it cannot be changed. */
  readonly change: string;
}

/** The distinct recordings the document's segments draw on, in its own order. */
function recordingsOf(document: EditDocument): readonly string[] {
  const found: string[] = [];
  for (const segment of document.segments) {
    // A segment naming a source the document does not declare is skipped, as
    // `recordings_of` skips it. `EditDocument::validate` refuses such a
    // document, so it is a shape no stored clip has.
    const recording = recordingOf(document, segment.source);
    if (recording !== undefined && !found.includes(recording)) {
      found.push(recording);
    }
  }
  return found;
}

/** Whether a crop takes the whole frame: `CropRect::FULL`, which is not a crop. */
function isFullFrame(crop: CropRect | null): boolean {
  return crop === null || (crop.x === 0 && crop.y === 0 && crop.width === 1 && crop.height === 1);
}

/** Whether a segment leaves the recording's pictures as they are. */
function isUntransformed(segment: Segment): boolean {
  return (
    segment.speed.numerator === segment.speed.denominator &&
    segment.rotation === 'none' &&
    isFullFrame(segment.crop)
  );
}

/**
 * Why `track` is a mix rather than one recorded stream, if it is.
 *
 * `resolve` rather than `monitor`: this is what `ExportPlan::of` asks
 * `document.track_output` — the **export** answer — and a solo left on
 * elsewhere is never part of it ([issue
 * #85](https://github.com/wildware-uk/clipped/issues/85)). Muting is the only
 * way a track reaches this window's export silent.
 */
function mixReasonOf(track: AudioTrack): MixReason | null {
  // The order is `plan_audio`'s, and it matters: a track that is both muted and
  // boosted is reported as silenced, because silence is what comes out of it.
  if (track.inputs.length > 1) {
    return 'severalInputs';
  }
  if (!resolve(track).audible) {
    return 'silenced';
  }
  if (track.gain_db !== 0) {
    return 'level';
  }
  if (track.fade_in !== 0 || track.fade_out !== 0) {
    return 'fades';
  }
  return null;
}

/**
 * What this clip's own edits already settle about copying it.
 *
 * The blockers come out in the order `ExportPlan::of` pushes them — the
 * recordings, then the audio tracks, then the overlays, then the segments — so
 * that this list and an export's own refusal read the same way round.
 *
 * The segments are examined only when the clip draws on exactly one recording,
 * which is `ExportPlan::of`'s own behaviour: `check_segments` runs against that
 * one recording's profile, and there is no such thing when a clip joins two. A
 * joined clip is already a re-encode, so nothing is lost by not looking.
 */
export function copyOutlook(document: EditDocument): CopyOutlook {
  const blockers: DocumentBlocker[] = [];
  const recordings = recordingsOf(document);

  if (recordings.length !== 1) {
    blockers.push({ kind: 'severalRecordings', recordings: recordings.length });
  }

  for (const [index, track] of document.audio_tracks.entries()) {
    if (track.inputs.length === 0) {
      // `EditDocument::validate` refuses this, and `ExportPlan::of` runs that
      // validation first, so the engine's answer is a refusal rather than a
      // plan. The window's reader deliberately does not validate
      // (`document.ts`), so it can be reached from stored text that no Clipped
      // build wrote.
      return {
        ok: false,
        problem:
          `The audio track “${track.name}” has no recorded stream feeding it, so this clip ` +
          'cannot be exported as it stands.',
      };
    }
    const reason = mixReasonOf(track);
    if (reason !== null) {
      blockers.push({ kind: 'trackNeedsMixing', track: index, reason });
    }
  }

  if (document.overlays.length > 0) {
    blockers.push({ kind: 'overlays', overlays: document.overlays.length });
  }

  if (recordings.length === 1) {
    for (const [index, segment] of document.segments.entries()) {
      if (!isUntransformed(segment)) {
        blockers.push({ kind: 'segmentTransformed', segment: index });
      }
    }
  }

  return { ok: true, blockers };
}

/** A level as the editor writes it, so the two agree about one track. */
function decibels(gainDb: number): string {
  return `${gainDb > 0 ? '+' : ''}${gainDb.toFixed(1)} dB`;
}

/** "1 recording", "2 recordings" — a count with the right noun on it. */
function count(n: number, singular: string, plural: string): string {
  return `${String(n)} ${n === 1 ? singular : plural}`;
}

/** A duration as seconds, the way a fade is talked about. */
function seconds(nanos: number): string {
  return `${(nanos / NANOS_PER_SECOND).toFixed(1)} s`;
}

/** Which of the three transformations a segment carries, listed. */
function transformationsOf(segment: Segment): string {
  const found: string[] = [];
  // A speed is source over output: `Speed::output_nanos` multiplies by the
  // denominator and divides by the numerator, so 2/1 plays two seconds of the
  // recording in one and is the *faster* of the two. Written the other way
  // round first, and `exportPlan.test.ts` said so.
  const { numerator, denominator } = segment.speed;
  if (numerator > denominator) {
    found.push('sped up');
  } else if (numerator < denominator) {
    found.push('slowed down');
  }
  if (!isFullFrame(segment.crop)) {
    found.push('cropped');
  }
  if (segment.rotation !== 'none') {
    found.push('rotated');
  }
  return found.join(' and ');
}

/** How a track fades, from the two figures the document carries. */
function fadesOf(track: AudioTrack): string {
  const ends: string[] = [];
  if (track.fade_in !== 0) {
    ends.push(`in over ${seconds(track.fade_in)}`);
  }
  if (track.fade_out !== 0) {
    ends.push(`out over ${seconds(track.fade_out)}`);
  }
  return ends.join(' and ');
}

/** What a track is called, or its position when the document left it unnamed. */
function trackName(track: AudioTrack, index: number): string {
  return track.name === '' ? `Track ${String(index + 1)}` : track.name;
}

/** One track's mix reason, worded against the track itself. */
function describeMix(track: AudioTrack, index: number, reason: MixReason): Reason {
  const name = trackName(track, index);
  switch (reason) {
    case 'severalInputs':
      return {
        what: `The “${name}” track sums ${count(track.inputs.length, 'recorded stream', 'recorded streams')}.`,
        change:
          'A sum has to be produced sample by sample. A track fed by one recorded stream is copied as it is.',
      };
    case 'silenced':
      // Muting is the only way a track reaches an export silent: a solo left
      // on elsewhere never does (issue #85), so there is only one sentence to
      // say here.
      return {
        what: `The “${name}” track is muted.`,
        change:
          'The clip keeps the track, so its silence has to be produced rather than copied. Make it audible again and it is copied.',
      };
    case 'level':
      return {
        what: `The “${name}” track plays at ${decibels(track.gain_db)} rather than the level it was recorded at.`,
        change: 'Set it back to as recorded and it is copied.',
      };
    case 'fades':
      return {
        what: `The “${name}” track fades ${fadesOf(track)}.`,
        change: 'A fade changes every sample it covers. Remove it and the track is copied.',
      };
  }
}

/**
 * A blocker in the words the dialog shows.
 *
 * The document is passed because the specifics are in it: which
 * transformations a segment carries, what level a track plays at, how many
 * streams it sums. The crate's `Display` cannot say those — it is writing for a
 * log — and a dialog that said "a track is a mix" would be telling somebody to
 * go and find out which one and why.
 */
export function describeBlocker(document: EditDocument, blocker: DocumentBlocker): Reason {
  switch (blocker.kind) {
    case 'severalRecordings':
      return blocker.recordings === 0
        ? {
            what: 'This clip has no material: no segment of it names a recording it declares.',
            change: 'There is nothing to export until it has some.',
          }
        : {
            what: `This clip joins ${count(blocker.recordings, 'recording', 'recordings')}.`,
            change:
              'Two recordings are two sets of stream descriptions and cannot share one container header. A clip taken from a single recording is copied.',
          };
    case 'trackNeedsMixing': {
      // The position came from the document these blockers were read out of, so
      // the track is there. The other branch is what a blocker read against a
      // different document would say: the reason without the specifics, rather
      // than specifics belonging to somebody else's track.
      const track = document.audio_tracks[blocker.track];
      return track === undefined
        ? {
            what: `Audio track ${String(blocker.track + 1)} of this clip is a mix rather than one recorded stream.`,
            change: 'A mix has to be produced sample by sample rather than copied.',
          }
        : describeMix(track, blocker.track, blocker.reason);
    }
    case 'overlays':
      return {
        what: `${count(blocker.overlays, 'piece', 'pieces')} of text ${blocker.overlays === 1 ? 'is' : 'are'} drawn over the picture.`,
        change: 'Text makes new pictures. Remove it and the recording’s own pictures are copied.',
      };
    case 'segmentTransformed': {
      const segment = document.segments[blocker.segment];
      // "transformed" is the crate's own word, and it is what is left to say
      // when the segment itself is not there to be read.
      const how = segment === undefined ? '' : transformationsOf(segment);
      return {
        what: `Segment ${String(blocker.segment + 1)} is ${how === '' ? 'transformed' : how}.`,
        change:
          'That makes new pictures. Set the segment back to the recording’s own speed, framing and rotation and it is copied.',
      };
    }
  }
}

/**
 * What deciding a copy still needs the recording for, in the words the dialog
 * shows.
 *
 * One entry per check `ExportPlan::of` makes against a `SourceProfile`, so this
 * is what is genuinely unknown here rather than a hedge: the keyframe at each
 * cut (`check_segments`), the codecs and the picture order (`check_video`), the
 * pictures a segment covers (`place_segments`), and the shape, which is asked
 * only when the document asks for one.
 */
export function checksNeedingTheRecording(document: EditDocument): readonly string[] {
  const checks = [
    'whether each cut falls on a picture a decoder can start at — a cut between keyframes cannot be copied without showing material the cut removed',
    'whether Clipped’s container writer can describe the recording’s codecs',
    'whether the recording stores its pictures in the order they are shown',
    'whether every segment covers at least one picture of the recording',
  ];
  if (document.aspect_ratio !== null) {
    checks.push(
      `whether the recording is ${String(document.aspect_ratio.width)}:${String(document.aspect_ratio.height)}, which is the shape this clip asks for`,
    );
  }
  return checks;
}
