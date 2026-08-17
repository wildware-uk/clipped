/**
 * The edit document, as the window reads it.
 *
 * An edit is metadata over recordings that are never touched: which recordings
 * to play, which parts of them, in which order, how loud each audio track is
 * and what text to draw over the picture. `crates/edit` owns the model and
 * `docs/editing.md` is its specification; this file is the reader the editor
 * screen parses one with.
 *
 * # Why a reader here at all
 *
 * `docs/editing.md` settles it: "a document crosses the IPC boundary as the
 * same JSON, rather than being converted into a second representation for the
 * desktop application". So the window reads the text `EditDocument::write`
 * produces, and this is the one place that knows its shape.
 *
 * What this is **not** is a second implementation of the model (AGENTS.md
 * section 55). It reads and refuses; it does not edit, validate the ranges a
 * document may hold, or write. The writer validates — `EditDocument::write`
 * runs validation before producing any text, so nothing unwritable reaches the
 * database — and the four operations that change a document live in
 * `crates/edit`, where their tests are.
 *
 * # Nothing here can touch a recording
 *
 * Neither can anything else in this window: it has no file-system permission at
 * all (`src-tauri/capabilities/default.json` grants three `core:` permissions).
 * That is the same inability `crates/edit` relies on rather than care taken at
 * each call site (AGENTS.md sections 56 and 57).
 *
 * # Refusing
 *
 * The compatibility table in `docs/editing.md` is the contract, and it is
 * followed here rather than restated: a document from a newer build is refused
 * by version rather than misread, one with no version is refused rather than
 * guessed at, and one carrying a field this build does not know is refused
 * rather than opened with the field silently dropped. Every refusal is a
 * sentence the screen shows, because a document that will not load has to say
 * why.
 */

/** The format version this build reads: `SCHEMA_VERSION` in `crates/edit`. */
export const EDIT_SCHEMA_VERSION = 2;

/** A ratio of two whole numbers of nanoseconds per nanosecond. */
export interface Speed {
  readonly numerator: number;
  readonly denominator: number;
}

/** A half-open range of a recording's own timeline, in nanoseconds. */
export interface SourceSpan {
  readonly start: number;
  readonly end: number;
}

/** A half-open range of the edited timeline, in nanoseconds. */
export interface OutputSpan {
  readonly start: number;
  readonly end: number;
}

/** A rectangle of the source frame, as fractions of it. */
export interface CropRect {
  readonly x: number;
  readonly y: number;
  readonly width: number;
  readonly height: number;
}

/** How far the picture is turned, clockwise. */
export type Rotation = 'none' | 'clockwise90' | 'clockwise180' | 'clockwise270';

/** The shape of the file an export would write. */
export interface AspectRatio {
  readonly width: number;
  readonly height: number;
}

/** A recording this edit draws on, named by the library's identifier for it. */
export interface Source {
  readonly id: number;
  readonly recording: string;
}

/** A piece of one recording, in the place it plays. */
export interface Segment {
  readonly source: number;
  readonly span: SourceSpan;
  readonly speed: Speed;
  readonly crop: CropRect | null;
  readonly rotation: Rotation;
}

/** One recorded stream feeding an output track. */
export interface TrackInput {
  readonly source: number;
  readonly stream: number;
}

/**
 * An audio track of the exported clip.
 *
 * Every field here is saved and reaches the export. Solo is deliberately not
 * one of them: [issue #85](https://github.com/wildware-uk/clipped/issues/85)
 * moved it out of `crates/edit`'s model and into `Solo`, a value the editor
 * holds beside the document and never inside it — `timeline.ts`'s `Solo` is
 * this window's mirror of that. Format 1 carried `soloed` here; format 2 does
 * not, and this build reads format 2.
 */
export interface AudioTrack {
  readonly name: string;
  readonly inputs: readonly TrackInput[];
  readonly gain_db: number;
  readonly muted: boolean;
  /** Nanoseconds of output time at the start of the clip. */
  readonly fade_in: number;
  /** Nanoseconds of output time at the end of the clip. */
  readonly fade_out: number;
}

/** A line of text over the picture, timed in output time. */
export interface TextOverlay {
  readonly text: string;
  readonly when: OutputSpan;
  readonly position: { readonly x: number; readonly y: number };
  readonly height_percent: number;
}

/** A non-destructive edit. */
export interface EditDocument {
  readonly schema_version: number;
  readonly title: string;
  readonly aspect_ratio: AspectRatio | null;
  readonly sources: readonly Source[];
  readonly segments: readonly Segment[];
  readonly audio_tracks: readonly AudioTrack[];
  readonly overlays: readonly TextOverlay[];
}

/** A document, or the sentence saying why it was refused. */
export type EditDocumentRead =
  | { readonly ok: true; readonly document: EditDocument }
  | { readonly ok: false; readonly problem: string };

/**
 * A refusal, carried as an exception so that a reader twelve objects deep can
 * name the field it refused without every level above returning a result.
 *
 * Caught at the one boundary below; nothing outside this file sees it.
 */
class Refusal extends Error {}

/** Refuses the document, naming what is wrong with it. */
function refuse(reason: string): never {
  throw new Refusal(reason);
}

/** The value at `path`, as an object, or a refusal naming the path. */
function object(value: unknown, path: string): Record<string, unknown> {
  if (typeof value !== 'object' || value === null || Array.isArray(value)) {
    refuse(`${path} should be an object.`);
  }
  return value as Record<string, unknown>;
}

/**
 * Refuses any key of `value` that is not in `known`.
 *
 * `crates/edit` puts `deny_unknown_fields` on every structure it reads, and
 * `docs/editing.md` argues why: an older build that opened a newer document,
 * dropped the field it did not understand and wrote that back would lose
 * whatever the user had set. Refusing to open beats opening and discarding.
 */
function noUnknownFields(value: Record<string, unknown>, known: readonly string[], path: string) {
  for (const key of Object.keys(value)) {
    if (!known.includes(key)) {
      refuse(`${path} carries "${key}", which this build does not understand.`);
    }
  }
}

/** A whole number of nanoseconds: what every time in the document is. */
function nanos(value: unknown, path: string): number {
  if (typeof value !== 'number' || !Number.isSafeInteger(value) || value < 0) {
    refuse(`${path} should be a whole number of nanoseconds.`);
  }
  return value;
}

/** A whole number that counts something: an identifier, a stream, a ratio. */
function whole(value: unknown, path: string): number {
  if (typeof value !== 'number' || !Number.isSafeInteger(value) || value < 0) {
    refuse(`${path} should be a whole number.`);
  }
  return value;
}

/** A real number: a level in decibels, a fraction of a frame. */
function real(value: unknown, path: string): number {
  if (typeof value !== 'number' || !Number.isFinite(value)) {
    refuse(`${path} should be a number.`);
  }
  return value;
}

/** A string, however short. */
function text(value: unknown, path: string): string {
  if (typeof value !== 'string') {
    refuse(`${path} should be text.`);
  }
  return value;
}

/** A boolean, which every flag in the document has a default for. */
function flag(value: unknown, path: string, fallback: boolean): boolean {
  if (value === undefined) {
    return fallback;
  }
  if (typeof value !== 'boolean') {
    refuse(`${path} should be true or false.`);
  }
  return value;
}

/** An array, whose elements are read one at a time so a failure names one. */
function list<T>(value: unknown, path: string, read: (item: unknown, path: string) => T): T[] {
  if (value === undefined) {
    return [];
  }
  if (!Array.isArray(value)) {
    refuse(`${path} should be a list.`);
  }
  return value.map((item, index) => read(item, `${path}[${index}]`));
}

function readSpan(value: unknown, path: string): SourceSpan {
  const span = object(value, path);
  noUnknownFields(span, ['start', 'end'], path);
  return { start: nanos(span.start, `${path}.start`), end: nanos(span.end, `${path}.end`) };
}

function readSpeed(value: unknown, path: string): Speed {
  if (value === undefined) {
    return { numerator: 1, denominator: 1 };
  }
  const speed = object(value, path);
  noUnknownFields(speed, ['numerator', 'denominator'], path);
  return {
    numerator: whole(speed.numerator, `${path}.numerator`),
    denominator: whole(speed.denominator, `${path}.denominator`),
  };
}

function readCrop(value: unknown, path: string): CropRect | null {
  if (value === undefined || value === null) {
    return null;
  }
  const crop = object(value, path);
  noUnknownFields(crop, ['x', 'y', 'width', 'height'], path);
  return {
    x: real(crop.x, `${path}.x`),
    y: real(crop.y, `${path}.y`),
    width: real(crop.width, `${path}.width`),
    height: real(crop.height, `${path}.height`),
  };
}

const ROTATIONS: readonly Rotation[] = ['none', 'clockwise90', 'clockwise180', 'clockwise270'];

function readRotation(value: unknown, path: string): Rotation {
  if (value === undefined) {
    return 'none';
  }
  const found = ROTATIONS.find((rotation) => rotation === value);
  if (found === undefined) {
    refuse(`${path} should be one of ${ROTATIONS.join(', ')}.`);
  }
  return found;
}

function readSegment(value: unknown, path: string): Segment {
  const segment = object(value, path);
  noUnknownFields(segment, ['source', 'span', 'speed', 'crop', 'rotation'], path);
  return {
    source: whole(segment.source, `${path}.source`),
    span: readSpan(segment.span, `${path}.span`),
    speed: readSpeed(segment.speed, `${path}.speed`),
    crop: readCrop(segment.crop, `${path}.crop`),
    rotation: readRotation(segment.rotation, `${path}.rotation`),
  };
}

function readSource(value: unknown, path: string): Source {
  const source = object(value, path);
  noUnknownFields(source, ['id', 'recording'], path);
  return {
    id: whole(source.id, `${path}.id`),
    recording: text(source.recording, `${path}.recording`),
  };
}

function readTrackInput(value: unknown, path: string): TrackInput {
  const input = object(value, path);
  noUnknownFields(input, ['source', 'stream'], path);
  return {
    source: whole(input.source, `${path}.source`),
    stream: whole(input.stream, `${path}.stream`),
  };
}

function readAudioTrack(value: unknown, path: string): AudioTrack {
  const track = object(value, path);
  noUnknownFields(track, ['name', 'inputs', 'gain_db', 'muted', 'fade_in', 'fade_out'], path);
  return {
    name: text(track.name, `${path}.name`),
    inputs: list(track.inputs, `${path}.inputs`, readTrackInput),
    gain_db: track.gain_db === undefined ? 0 : real(track.gain_db, `${path}.gain_db`),
    muted: flag(track.muted, `${path}.muted`, false),
    fade_in: track.fade_in === undefined ? 0 : nanos(track.fade_in, `${path}.fade_in`),
    fade_out: track.fade_out === undefined ? 0 : nanos(track.fade_out, `${path}.fade_out`),
  };
}

function readOverlay(value: unknown, path: string): TextOverlay {
  const overlay = object(value, path);
  noUnknownFields(overlay, ['text', 'when', 'position', 'height_percent'], path);
  const when = object(overlay.when, `${path}.when`);
  noUnknownFields(when, ['start', 'end'], `${path}.when`);
  const position = object(overlay.position, `${path}.position`);
  noUnknownFields(position, ['x', 'y'], `${path}.position`);
  return {
    text: text(overlay.text, `${path}.text`),
    when: {
      start: nanos(when.start, `${path}.when.start`),
      end: nanos(when.end, `${path}.when.end`),
    },
    position: {
      x: real(position.x, `${path}.position.x`),
      y: real(position.y, `${path}.position.y`),
    },
    height_percent: whole(overlay.height_percent, `${path}.height_percent`),
  };
}

function readAspectRatio(value: unknown, path: string): AspectRatio | null {
  if (value === undefined || value === null) {
    return null;
  }
  const ratio = object(value, path);
  noUnknownFields(ratio, ['width', 'height'], path);
  return {
    width: whole(ratio.width, `${path}.width`),
    height: whole(ratio.height, `${path}.height`),
  };
}

/**
 * Reads the text a clip's edit document is stored as.
 *
 * The version is read out of the raw JSON before anything else is trusted,
 * which is the whole point of it: a document from a newer build may have a
 * shape this one cannot read at all, and it still has to be refused as a
 * version rather than as a parse failure.
 */
export function readEditDocument(stored: string): EditDocumentRead {
  let parsed: unknown;
  try {
    parsed = JSON.parse(stored);
  } catch (error) {
    return {
      ok: false,
      problem: `This clip's edit document is not valid JSON: ${(error as Error).message}`,
    };
  }

  try {
    const document = object(parsed, 'An edit document');
    const version: unknown = document.schema_version;
    if (typeof version !== 'number' || !Number.isSafeInteger(version)) {
      refuse('This edit document does not say which format it is in, so it cannot be read.');
    }
    if (version > EDIT_SCHEMA_VERSION) {
      refuse(
        `This edit document is in format ${String(version)} and this build of Clipped reads ` +
          `${String(EDIT_SCHEMA_VERSION)}. Update Clipped to open it. Nothing has been changed.`,
      );
    }
    if (version < EDIT_SCHEMA_VERSION) {
      // The migration chain lives in `crates/edit`, which converts in memory and
      // tells its caller it did. This window is not that caller: it cannot store
      // the result, so converting here would leave the editor showing a document
      // nothing agreed to.
      refuse(
        `This edit document is in format ${String(version)}, which this window cannot convert. ` +
          'Nothing has been changed.',
      );
    }

    noUnknownFields(
      document,
      [
        'schema_version',
        'title',
        'aspect_ratio',
        'sources',
        'segments',
        'audio_tracks',
        'overlays',
      ],
      'This edit document',
    );

    // `sources` and `segments` carry no default in the model, so a document
    // without them is a document this build cannot draw rather than an empty
    // one. The empty *clip* — no sources, no segments — is written as two empty
    // lists and is valid; see `docs/editing.md`.
    for (const required of ['sources', 'segments']) {
      if (document[required] === undefined) {
        refuse(`This edit document has no "${required}", so it cannot be read.`);
      }
    }

    return {
      ok: true,
      document: {
        schema_version: version,
        title: text(document.title, 'The title'),
        aspect_ratio: readAspectRatio(document.aspect_ratio, 'aspect_ratio'),
        sources: list(document.sources, 'sources', readSource),
        segments: list(document.segments, 'segments', readSegment),
        audio_tracks: list(document.audio_tracks, 'audio_tracks', readAudioTrack),
        overlays: list(document.overlays, 'overlays', readOverlay),
      },
    };
  } catch (error) {
    if (error instanceof Refusal) {
      return { ok: false, problem: error.message };
    }
    throw error;
  }
}

/** The recording a segment plays, or `undefined` if the document declares none. */
export function recordingOf(document: EditDocument, source: number): string | undefined {
  return document.sources.find((declared) => declared.id === source)?.recording;
}
