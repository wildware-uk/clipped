/**
 * Game events as a timeline draws them (issues #71 and #65).
 *
 * A plugin reports that something happened at a moment on a recording's
 * timeline (`docs/plugin-api.md`); `clipped_library::events` turns that moment
 * into a position in one of the session's files. What arrives here is the
 * result: a recording, a position in it, and the two fields a mark is drawn and
 * named from.
 *
 * Two screens read it. The Editor places these marks on an edit document
 * (`editor/timeline.ts`, issue #71); the playback screen places them on the
 * recording itself (`recordingMarks.ts`, issue #65). Neither is allowed its
 * own vocabulary, which is why this file sits above both of them.
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

/**
 * Who put a mark on a timeline.
 *
 * Three, because they are three different claims and a screen that ran them
 * together would let one speak for another: `plugin` is a game integration
 * saying what happened in the game, `clipped` is the application saying what it
 * did, and `you` is a name somebody typed. `crates/plugins/src/report.rs`
 * refuses a plugin that tries to report the third — the whole reason that guard
 * exists is so that this distinction can be drawn here.
 */
export type MarkOrigin = 'you' | 'clipped' | 'plugin';

/** The application's own source, `EventSource::APPLICATION`. */
export const APPLICATION_SOURCE = 'clipped';

/** What `EventKind::UserLabelled` serialises with, `USER_LABEL_PREFIX`. */
export const USER_LABEL_PREFIX = 'user:';

/** What a mark is called, and who it came from. */
export interface MarkDescription {
  /** What to call it. A user's own words, a vocabulary label, or the raw tag. */
  readonly label: string;
  /** Which of the three put it there. */
  readonly origin: MarkOrigin;
  /**
   * Who, in words, to be read after the label: `reported by the
   * counter-strike-2 plugin`.
   *
   * A phrase rather than a name because it is what a screen reader announces,
   * and "counter-strike-2" on its own does not say that a plugin claimed it.
   */
  readonly by: string;
  /** The one-word category, for a legend and for a filter. */
  readonly word: string;
}

/**
 * What one mark is called and who it came from, decided from the two fields the
 * recorder sent and from nothing else.
 *
 * # The kind is read before the source, and that order is the point
 *
 * A user's label is *written by the application*, so its `source` is `clipped`
 * exactly like a mark Clipped placed on its own account
 * (`crates/events/src/event.rs`). The source alone cannot tell "you typed this"
 * from "Clipped noticed this"; the kind can, because `EventKind::UserLabelled`
 * has a wire form of its own — `user:` and then the words
 * (`crates/events/src/kind.rs`). Reading the source first would attribute
 * somebody's own note to the application.
 *
 * # Nothing here is recomputed
 *
 * `EventSource::plugin` refuses `clipped` and anything under `clipped.`
 * (`crates/events/src/event.rs`), so "not the application" *is* "a plugin" —
 * this window is reading a distinction the producer already made rather than
 * guessing at one. Anything else is a plugin identifier, verbatim, and it is
 * shown verbatim: a manifest identifier is what a plugin author would search
 * for when a mark of theirs is in the wrong place.
 */
export function describeMark(mark: EventMark): MarkDescription {
  if (mark.kind.startsWith(USER_LABEL_PREFIX)) {
    const label = mark.kind.slice(USER_LABEL_PREFIX.length);
    return {
      // The raw tag when there are no words after the prefix. A blank mark on a
      // timeline is a mark nobody can find again.
      label: label === '' ? mark.kind : label,
      origin: 'you',
      by: 'labelled by you',
      word: 'Yours',
    };
  }

  const { label } = describeKind(mark.kind);

  if (mark.source === APPLICATION_SOURCE || mark.source.startsWith(`${APPLICATION_SOURCE}.`)) {
    return { label, origin: 'clipped', by: 'marked by Clipped', word: 'Clipped' };
  }

  return { label, origin: 'plugin', by: `reported by the ${mark.source} plugin`, word: 'Plugin' };
}
