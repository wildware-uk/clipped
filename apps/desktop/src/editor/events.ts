/**
 * Game events as the timeline draws them (issue #71).
 *
 * A plugin reports that something happened at a moment on a recording's
 * timeline (`docs/plugin-api.md`); `clipped_library::events` turns that moment
 * into a position in one of the session's files. What arrives here is the
 * result: a recording, a position in it, and the two fields a mark is drawn and
 * named from.
 *
 * # The vocabulary is open, and this file is the reason that matters
 *
 * `clipped_events::EventKind` is a closed list of concepts the application acts
 * on, plus two ways of carrying a name that is not on it: `Custom`, which a
 * plugin namespaced with its own identifier, and `Unrecognised`, which is a
 * kind added to the vocabulary after this build shipped. Both reach a timeline,
 * and both **must be drawn**. A screen that knew fourteen kinds and quietly
 * skipped the fifteenth would show a user fewer marks than the recorder found
 * and say nothing about it, which is the failure AGENTS.md section 27 is about.
 *
 * So nothing below filters on the list. {@link describeKind} answers for any
 * string, the list decides only what a mark is *called*, and a kind this build
 * has never met is drawn identically and labelled with the tag it arrived with.
 *
 * # What is not here
 *
 * The event's payload. `data` is the plugin's own detail — a weapon, a victim,
 * a hero identifier — and nothing above the plugin interprets it
 * (`crates/events/src/event.rs`). A timeline that switched on a payload key to
 * decide what a mark means would have moved a game's protocol into the
 * interface.
 */

/** How a kind reads, and what this build knows about it. */
export interface KindDescription {
  /** What to call it. The tag itself when this build has no name for it. */
  readonly label: string;
  /** Whether it is one of the kinds this build acts on. */
  readonly known: boolean;
  /** The plugin that invented it, for a namespaced custom name. */
  readonly plugin: string | null;
}

/**
 * The closed vocabulary, and what each one is called.
 *
 * The keys are `clipped_events::EventKind::as_str`, in the order that
 * enumeration declares them, so the filter below lists kinds in a fixed order
 * rather than in whichever order a session happened to produce them.
 * `events.test.ts` holds this list to the crate's.
 */
const KIND_LABELS: ReadonlyMap<string, string> = new Map([
  ['game_started', 'Game started'],
  ['game_ended', 'Game ended'],
  ['match_started', 'Match started'],
  ['match_ended', 'Match ended'],
  ['kill', 'Kill'],
  ['death', 'Death'],
  ['assist', 'Assist'],
  ['round_started', 'Round started'],
  ['round_ended', 'Round ended'],
  ['win', 'Win'],
  ['loss', 'Loss'],
  ['score', 'Score'],
  ['goal', 'Goal'],
  ['achievement', 'Achievement'],
]);

/** The kinds this build acts on, in the order the vocabulary declares them. */
export const KNOWN_KINDS: readonly string[] = [...KIND_LABELS.keys()];

/**
 * What to call `kind`, whether this build knows it, and who invented it.
 *
 * Never fails and never refuses. A namespaced name is a plugin's own word — the
 * syntactic rule `crates/events/src/kind.rs` uses to keep the open variant from
 * swallowing the closed one — so it is shown verbatim and attributed, rather
 * than prettified into something the plugin did not write.
 */
export function describeKind(kind: string): KindDescription {
  const label = KIND_LABELS.get(kind);
  if (label !== undefined) {
    return { label, known: true, plugin: null };
  }
  const dot = kind.indexOf('.');
  return {
    label: kind,
    known: false,
    plugin: dot > 0 ? kind.slice(0, dot) : null,
  };
}

/** One event, placed in one recording. */
export interface EventMark {
  /** The library identifier of the recording it is in. */
  readonly recording: string;
  /** How far into that recording's file it is, in nanoseconds. */
  readonly at: number;
  /** The kind, exactly as it was stored. */
  readonly kind: string;
  /** Who reported it: a plugin's identifier, or `clipped` for the application. */
  readonly source: string;
}

/**
 * The kinds these marks carry, known ones first in vocabulary order and the
 * rest after them in alphabetical order.
 *
 * The order is fixed rather than "as encountered" so that the filter does not
 * rearrange itself when a session produces its events in a different order, and
 * unknown kinds are last rather than interleaved so that the list a user reads
 * first is the one they have names for.
 */
export function kindsPresent(marks: readonly EventMark[]): readonly string[] {
  const present = new Set(marks.map((mark) => mark.kind));
  const known = KNOWN_KINDS.filter((kind) => present.has(kind));
  const rest = [...present]
    .filter((kind) => !KIND_LABELS.has(kind))
    .sort((a, b) => (a < b ? -1 : 1));
  return [...known, ...rest];
}

/** How many marks there are of each kind. */
export function countByKind(marks: readonly EventMark[]): ReadonlyMap<string, number> {
  const counts = new Map<string, number>();
  for (const mark of marks) {
    counts.set(mark.kind, (counts.get(mark.kind) ?? 0) + 1);
  }
  return counts;
}
