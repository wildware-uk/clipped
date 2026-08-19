import type { LibraryEventLane } from '@clipped/shared';

import type { EventMark } from '../events';
import schema from '../../../../packages/shared/src/ipc/protocol-schema.json';

/**
 * The marks the timeline is tested against, taken from the recorder's own
 * exemplar rather than typed out here.
 *
 * `protocol-schema.json` is written by `cargo run -p clipped-ipc --bin
 * protocol-schema` from the Rust types themselves, and one of the frames in it
 * is a `library_events` reply the recorder built and its own deserialiser read
 * back. Everything below is that frame, or a mark of that shape moved to a
 * different second — so a field renamed in `crates/ipc/src/library.rs`, or a
 * `kind` that stops being a bare string, fails these tests instead of leaving
 * them agreeing with a protocol that has moved.
 *
 * It is the standard PR #647 and PR #652 set, for the same reason: a hand-typed
 * literal is a copy of the protocol that nothing keeps honest.
 */

/** A sample frame the Rust build produced, by the name it recorded it under. */
function sample(name: string): Record<string, unknown> {
  const samples = schema.samples as { name: string; frame: Record<string, unknown> }[];
  const found = samples.find((candidate) => candidate.name === name);
  if (found === undefined) {
    throw new Error(
      `protocol-schema.json has no sample called "${name}"; regenerate it, or the name has moved`,
    );
  }
  return found.frame;
}

/** The recorder's own `library_events` reply, whole. */
export const SAMPLE_LANE: LibraryEventLane = (
  sample("the marks on one recording's timeline") as {
    outcome: { ok: { lane: LibraryEventLane } };
  }
).outcome.ok.lane;

/** The marks in it: one standard kind, and one a plugin invented. */
export const SAMPLE_MARKS: readonly EventMark[] = SAMPLE_LANE.marks;

/**
 * The first mark of the exemplar — a `kill` a shipped integration reports.
 *
 * The one kind on this timeline that a real recorder really does produce today:
 * `plugins/cs2/src/derive.rs` reports it, `report.rs` stamps the plugin's
 * identifier on it as the source, and `library_events` answers with it.
 */
export const PLUGIN_MARK: EventMark = SAMPLE_MARKS[0] ?? missing(0);

/** The second — a namespaced `Custom` name, which is the open half of the vocabulary. */
export const CUSTOM_MARK: EventMark = SAMPLE_MARKS[1] ?? missing(1);

/** The recording the exemplar's marks are on, as the library identifies it. */
export const SAMPLE_RECORDING = PLUGIN_MARK.recording;

function missing(index: number): never {
  throw new Error(`the library_events exemplar has no mark ${String(index)}`);
}

/** The exemplar's mark, moved to `seconds` into the recording. */
export function markAt(seconds: number, of: EventMark = PLUGIN_MARK): EventMark {
  return { ...of, at: Math.round(seconds * 1_000_000_000) };
}

/**
 * How long the recording the responsiveness fixture is placed on runs for:
 * three hours.
 *
 * "Multi-hour" is issue #65's own word. Three is what a sitting of a game
 * somebody is actually playing looks like, and it is long enough that the
 * failure being guarded against — a marker per mark — is thousands of nodes
 * rather than dozens.
 */
export const LONG_RECORDING_SECONDS = 3 * 60 * 60;

/**
 * How many marks that recording carries: ten thousand.
 *
 * Deliberately more than anything Clipped ships produces. A three-hour
 * Counter-Strike sitting is on the order of a thousand marks — a kill, a death,
 * an assist and two round boundaries a round, ninety-odd rounds — so ten
 * thousand is an order of magnitude past the worst real case. It is the same
 * figure `docs/library.md` measures the index against and `virtualWindow.test.ts`
 * measures the Library's table against, so a number here and a number there
 * describe libraries of the same size.
 */
export const MANY_MARKS_COUNT = 10_000;

/**
 * Ten thousand marks across three hours, bunched the way a game's really are.
 *
 * The gaps cycle rather than being even: four short ones and one long one, so
 * that marks arrive in bursts a few tenths of a second apart with quiet between
 * them. An evenly-spread fixture would be the one case bucketing is easiest on —
 * every marker alone in its column — and would say nothing about the case that
 * actually happens, which is thirty marks inside one round landing on the same
 * five pixels.
 */
export const MANY_MARKS: readonly EventMark[] = manyMarks();

function manyMarks(): readonly EventMark[] {
  /** Seconds between one mark and the next, cycled; they average 1.08. */
  const gaps = [0.08, 0.12, 0.2, 0.48, 4.52];
  const kinds = [PLUGIN_MARK, CUSTOM_MARK];
  const marks: EventMark[] = [];
  let at = 0;
  for (let index = 0; index < MANY_MARKS_COUNT; index += 1) {
    at += gaps[index % gaps.length] ?? 1;
    marks.push(markAt(Math.min(at, LONG_RECORDING_SECONDS), kinds[index % kinds.length]));
  }
  return marks;
}
