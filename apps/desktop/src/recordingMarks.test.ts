// @vitest-environment node
//
// Where a recording's marks are drawn, where clicking one goes, and what a
// multi-hour recording of them costs (issue #65).
//
// Three properties, one per acceptance criterion, and each is written against
// the way it would pass for the wrong reason:
//
// - **"Clicking a marker seeks accurately."** A build that seeks to zero every
//   time, or to the middle of the column a marker was drawn in, passes any test
//   that only asks whether something was sought. So every case below compares
//   the seek target against *that mark's own nanoseconds*, and says which mark
//   and which position in the failure.
// - **"A multi-hour recording with many markers stays responsive."** A build
//   that draws every mark at the same position draws exactly as few nodes as one
//   that buckets them properly. So the bound is asserted beside the positions
//   being distinct and increasing, and beside every mark being accounted for.
// - **"Marker source is identifiable."** The distinction is made in
//   `crates/events`, and the two constants this file reads it by are compared
//   against the Rust that declares them, so a rename there fails here.
//
// It runs in the node environment because two of its cases read a Rust source
// file, which is the same reason `editor/events.test.ts` does.

import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

import { describe, expect, it } from 'vitest';

import { APPLICATION_SOURCE, describeMark, USER_LABEL_PREFIX, type EventMark } from './events';
import { markerName, originsPresent, recordingTimeline, TIMELINE_COLUMNS } from './recordingMarks';
import {
  CUSTOM_MARK,
  LONG_RECORDING_SECONDS,
  MANY_MARKS,
  MANY_MARKS_COUNT,
  markAt,
  PLUGIN_MARK,
  SAMPLE_MARKS,
} from './test/eventMarkFixture';

/** A Rust source file of this workspace, as text. */
function crate(path: string): string {
  return readFileSync(fileURLToPath(new URL(`../../../${path}`, import.meta.url)), 'utf8');
}

describe('the two constants a mark’s source is read by', () => {
  /*
   * Both are strings this window compares against, and both are declared in
   * Rust. A `pub const` renamed or retuned there and copied nowhere is a window
   * that silently stops telling a plugin's mark from the application's — which
   * is the one distinction issue #65's third criterion is about — so they are
   * read out of the crate rather than asserted to be what they were.
   */
  it('is the source crates/events reserves for the application itself', () => {
    const source = crate('crates/events/src/kind.rs');
    const declared = /pub const RESERVED_NAMESPACE: &str = "([^"]+)";/.exec(source);

    expect(declared, 'RESERVED_NAMESPACE has moved in crates/events/src/kind.rs').not.toBeNull();
    expect(APPLICATION_SOURCE).toBe(declared?.[1]);
  });

  it('is the prefix crates/events gives a label somebody typed', () => {
    const source = crate('crates/events/src/kind.rs');
    const declared = /pub const USER_LABEL_PREFIX: &str = "([^"]+)";/.exec(source);

    expect(declared, 'USER_LABEL_PREFIX has moved in crates/events/src/kind.rs').not.toBeNull();
    expect(USER_LABEL_PREFIX).toBe(declared?.[1]);
  });

  /*
   * `EventSource::APPLICATION` is the reserved namespace, and `EventSource::
   * plugin` refuses it — which is what makes "not the application" mean "a
   * plugin" rather than being a guess this window makes.
   */
  it('is the one EventSource::plugin refuses, so a plugin cannot claim it', () => {
    const source = crate('crates/events/src/event.rs');

    expect(source).toContain("pub const APPLICATION: &'static str = RESERVED_NAMESPACE;");
    expect(source).toContain('InvalidSource::Reserved');
  });
});

describe('who a mark is attributed to', () => {
  it('names the plugin that reported it, for the mark a real recorder produces', () => {
    // The exemplar's own mark: `plugins/cs2` reports a kill, `report.rs` stamps
    // the manifest identifier on it, and `library_events` answers with it.
    const described = describeMark(PLUGIN_MARK);

    expect(described.origin).toBe('plugin');
    expect(described.by).toContain(PLUGIN_MARK.source);
    expect(described.by).toContain('plugin');
  });

  it('names the plugin for a kind it invented, too', () => {
    const described = describeMark(CUSTOM_MARK);

    expect(described.origin).toBe('plugin');
    // Verbatim: a namespaced name is the plugin's own word and prettifying it
    // would be showing somebody something the plugin did not write.
    expect(described.label).toBe(CUSTOM_MARK.kind);
    expect(described.by).toContain(CUSTOM_MARK.source);
  });

  it('names Clipped for the application’s own source, and for a component of it', () => {
    // Neither is produced by anything in this build — `EventSource::
    // application` has no production caller — but both are what the protocol
    // carries, and a reader that could not tell them from a plugin's would be
    // wrong the day one arrives.
    for (const source of [APPLICATION_SOURCE, `${APPLICATION_SOURCE}.input`]) {
      const described = describeMark({ ...PLUGIN_MARK, source });

      expect(described.origin, `a mark whose source is ${source}`).toBe('clipped');
      expect(described.by).toBe('marked by Clipped');
    }
  });

  it('reads the kind before the source, so a label of yours is not the application’s', () => {
    // The trap this ordering exists for: a user's label *is* written by the
    // application, so its source is `clipped` exactly like a mark Clipped
    // placed on its own account. Reading the source first would put somebody's
    // own words down as something Clipped noticed.
    const yours: EventMark = {
      ...PLUGIN_MARK,
      kind: `${USER_LABEL_PREFIX}Ace on Nuke`,
      source: APPLICATION_SOURCE,
    };

    const described = describeMark(yours);

    expect(described.origin).toBe('you');
    expect(described.label).toBe('Ace on Nuke');
    expect(described.by).toBe('labelled by you');
  });
});

describe('where a marker seeks to', () => {
  it('is the mark’s own position, to the nanosecond, for every mark of the exemplar', () => {
    const { markers } = recordingTimeline(SAMPLE_MARKS, 60);

    expect(markers).toHaveLength(SAMPLE_MARKS.length);
    for (const marker of markers) {
      expect(
        marker.atSeconds * 1_000_000_000,
        `${markerName(marker)} should seek to ${String(marker.mark.at)} ns`,
      ).toBe(marker.mark.at);
    }
  });

  it('is where the event happened, not the middle of the column it is drawn in', () => {
    /*
     * The failure this is written against. A build that seeks to the column it
     * drew a marker in passes "clicking seeks" and "the markers are at
     * different positions", and is wrong by up to one column — which on a
     * three-hour recording is forty-five seconds. So the mark is placed a long
     * way off its column's centre and the seek is compared with the mark.
     */
    const columns = 10;
    const duration = 100;
    // Column 3 covers 30..40 seconds; its centre is 35 and this is not it.
    const mark = markAt(31.25);

    const [marker] = recordingTimeline([mark], duration, columns).markers;

    expect(marker?.atSeconds, 'the marker should seek to 31.25s, where the kill was').toBe(31.25);
    expect(marker?.atSeconds, 'not to 35s, the middle of the column it is drawn in').not.toBe(35);
  });

  it('is the earliest mark under a marker, when several share one', () => {
    const columns = 10;
    const duration = 100;
    // All three are inside column 3, so one marker carries them.
    const marks = [markAt(34), markAt(31.5), markAt(39.75)];

    const { markers } = recordingTimeline(marks, duration, columns);

    expect(markers).toHaveLength(1);
    expect(markers[0]?.count).toBe(3);
    expect(markers[0]?.atSeconds, 'a marker seeks to the first thing under it').toBe(31.5);
  });

  it('says which mark, when, and who reported it', () => {
    const [marker] = recordingTimeline([markAt(3723.456)], LONG_RECORDING_SECONDS).markers;

    expect(marker).toBeDefined();
    expect(markerName(marker!)).toBe(
      `Kill at 1:02:03.456, reported by the ${PLUGIN_MARK.source} plugin`,
    );
  });

  it('says how many more are under a marker that carries several', () => {
    const marks = [markAt(10), markAt(10.1), markAt(10.2)];

    const [marker] = recordingTimeline(marks, 100, 10).markers;

    expect(marker).toBeDefined();
    expect(markerName(marker!)).toContain('and 2 more marks here');
  });

  it('attributes a marker covering marks that disagree to neither of them', () => {
    // A plugin's kill and a label of somebody's own, close enough together to
    // land in one column. Announcing the plugin's phrase over both would be the
    // integration putting words in the user's mouth that `report.rs` refuses to
    // let it do directly.
    const marks = [
      markAt(10),
      { ...markAt(10.1), kind: `${USER_LABEL_PREFIX}my best round`, source: APPLICATION_SOURCE },
    ];

    const [marker] = recordingTimeline(marks, 100, 10).markers;

    expect(marker?.origin).toBe('several');
    expect(markerName(marker!)).toContain('from several sources');
  });
});

describe('a three-hour recording with ten thousand marks', () => {
  /*
   * Issue #65's second criterion, measured the way `virtualWindow.test.ts`
   * measures issue #60's: **no frame rate is claimed**, because nothing in this
   * process can paint one. What is measured is the property a frame rate
   * depends on — how many things a browser is asked to lay out — against a
   * fixture whose size is written down: ten thousand marks across three hours,
   * bunched, which is an order of magnitude more than any integration Clipped
   * ships produces.
   */
  const placed = recordingTimeline(MANY_MARKS, LONG_RECORDING_SECONDS);

  it('is more marks than a marker each could be drawn for', () => {
    // The premise. Without it, every assertion below would be true of a build
    // that drew one node per mark.
    expect(MANY_MARKS).toHaveLength(MANY_MARKS_COUNT);
    expect(placed.total).toBe(MANY_MARKS_COUNT);
    expect(MANY_MARKS_COUNT).toBeGreaterThan(TIMELINE_COLUMNS * 10);
  });

  it('draws no more markers than there are columns, however many marks there are', () => {
    expect(placed.markers.length).toBeLessThanOrEqual(TIMELINE_COLUMNS);
  });

  it('loses none of them: every mark is under exactly one marker', () => {
    const under = placed.markers.reduce((total, marker) => total + marker.count, 0);

    expect(under + placed.offRecording).toBe(MANY_MARKS_COUNT);
    expect(placed.offRecording).toBe(0);
  });

  it('draws them at distinct, increasing positions rather than on top of one another', () => {
    /*
     * The failure this suite exists to catch. "Ten thousand marks became two
     * hundred and forty markers" is equally true of a build that piled them all
     * at zero, and that build is a timeline with one clickable thing on it.
     */
    let previous = -1;
    for (const marker of placed.markers) {
      expect(
        marker.fraction,
        `${markerName(marker)} is drawn at ${String(marker.fraction)}, not after ${String(previous)}`,
      ).toBeGreaterThan(previous);
      expect(marker.fraction).toBeLessThanOrEqual(1);
      previous = marker.fraction;
    }
  });

  it('spreads them across the whole recording, not over the start of it', () => {
    const first = placed.markers[0];
    const last = placed.markers[placed.markers.length - 1];

    expect(first?.fraction).toBeLessThan(0.01);
    expect(last?.fraction).toBeGreaterThan(0.99);
  });

  it('still seeks each of them to a mark’s own nanosecond', () => {
    for (const marker of placed.markers) {
      expect(
        Math.round(marker.atSeconds * 1_000_000_000),
        `${markerName(marker)} should seek to ${String(marker.mark.at)} ns`,
      ).toBe(marker.mark.at);
    }
  });

  it('does not depend on the order the recorder sent them in', () => {
    // `library_events` answers `ORDER BY at_nanos`, and nothing here leans on
    // it: the earliest mark in a column is found by comparison.
    const reversed = [...MANY_MARKS].reverse();

    const backwards = recordingTimeline(reversed, LONG_RECORDING_SECONDS);

    expect(backwards.markers.map((marker) => marker.atSeconds)).toEqual(
      placed.markers.map((marker) => marker.atSeconds),
    );
  });
});

describe('marks the recorder placed outside the file', () => {
  it('counts them rather than drawing them at the ends', () => {
    /*
     * `library_events` subtracts a recording's start from an event's session
     * time and clamps nothing, so a session whose spans were recorded oddly can
     * answer with a position before the file begins or after it ends. Drawing
     * those at zero and at the end would put marks on the timeline that are not
     * in the recording.
     */
    const marks = [markAt(-4), markAt(10), markAt(200)];

    const placed = recordingTimeline(marks, 100, 10);

    expect(placed.total).toBe(3);
    expect(placed.offRecording).toBe(2);
    expect(placed.markers).toHaveLength(1);
    expect(placed.markers[0]?.atSeconds).toBe(10);
  });

  it('keeps a mark at the very last instant of the recording', () => {
    const placed = recordingTimeline([markAt(100)], 100, 10);

    expect(placed.offRecording).toBe(0);
    expect(placed.markers).toHaveLength(1);
    expect(placed.markers[0]?.fraction).toBe(1);
  });

  it('places nothing at all against a length nothing has measured', () => {
    for (const duration of [0, Number.NaN, Number.POSITIVE_INFINITY, -1]) {
      const placed = recordingTimeline(SAMPLE_MARKS, duration);

      expect(placed.markers, `a recording of ${String(duration)} seconds`).toHaveLength(0);
      expect(placed.total).toBe(SAMPLE_MARKS.length);
    }
  });
});

describe('the legend', () => {
  it('lists only the origins this recording actually has', () => {
    const { markers } = recordingTimeline(SAMPLE_MARKS, 60);

    // Both exemplar marks are a plugin's, so the legend is one row: a row for
    // Clipped or for a label of yours would be a legend entry with nothing
    // under it, which is the empty lane this screen refuses to draw.
    expect(originsPresent(markers)).toEqual([{ origin: 'plugin', markers: 2, marks: 2 }]);
  });

  it('counts the marks under each origin, not just the markers drawn for them', () => {
    const marks = [markAt(10), markAt(10.1), markAt(90)];

    const { markers } = recordingTimeline(marks, 100, 10);

    expect(originsPresent(markers)).toEqual([{ origin: 'plugin', markers: 2, marks: 3 }]);
  });

  it('is in a fixed order, whichever origin a recording met first', () => {
    const marks = [
      { ...markAt(10), kind: `${USER_LABEL_PREFIX}mine`, source: APPLICATION_SOURCE },
      { ...markAt(50), source: APPLICATION_SOURCE },
      markAt(90),
    ];

    const { markers } = recordingTimeline(marks, 100, 10);

    expect(originsPresent(markers).map((entry) => entry.origin)).toEqual([
      'plugin',
      'clipped',
      'you',
    ]);
  });
});
