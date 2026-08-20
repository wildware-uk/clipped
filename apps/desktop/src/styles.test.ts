/**
 * Every `clipped-*` class the window names exists in the stylesheet
 * ([issue #688](https://github.com/wildware-uk/clipped/issues/688)).
 *
 * The window names its styles as strings, and nothing checked them. A typo — or
 * a class somebody writes because it sounds like it ought to exist — renders as
 * unstyled markup, and every other check stays green: `tsc` sees a string,
 * `eslint` sees a string, and Testing Library queries by role and text, neither
 * of which notices that a control has no styling.
 *
 * It is not only cosmetic. Building #245's controls I wrote five classes from
 * memory and none of them existed; one was carrying a screen-reader-only label,
 * so without it the text "Name for Counter-Strike 2" would have rendered
 * visibly inside a table cell. `tsc`, `eslint`, `prettier` and 1092 tests were
 * all green with every one of them in the tree.
 *
 * # Both sides are derived from their own source
 *
 * Neither the used set nor the defined set is a list kept here. A list would be
 * the third place to forget, which is the mistake this exists to catch — the
 * same shape as the recorder's
 * `every_capability_the_window_gates_on_is_one_this_recorder_advertises`.
 *
 * # A check that reads nothing must fail
 *
 * The floors below are not decoration. A refactor that moves the window, or a
 * glob that stops matching, would otherwise turn this into a test that passes
 * by examining an empty set — which reads as coverage and is the opposite.
 */

import { readdirSync, readFileSync } from 'node:fs';
import { join } from 'node:path';
import { describe, expect, it } from 'vitest';

/** Where the window's markup lives. */
const MARKUP = [join(__dirname), join(__dirname, '..', '..', '..', 'packages', 'ui', 'src')];

/** Where the classes are defined. */
const STYLESHEETS = join(__dirname, '..', '..', '..', 'packages', 'ui', 'src');

/**
 * How many classes are built at runtime and so cannot be checked from the
 * markup alone.
 *
 * Asserted exactly rather than ignored. One of these is
 * `` `clipped-marks__glyph--${origin}` ``, whose four spellings are all defined
 * — but no reader of the markup can know that, so the honest thing is to say
 * how many there are and notice when the number moves. A silent skip would be
 * machinery that looks like coverage and quietly covers less.
 */
const BUILT_AT_RUNTIME = 1;

/** Every `.tsx` and `.ts` under a directory that is not a test. */
function sources(directory: string): string[] {
  const found: string[] = [];
  for (const entry of readdirSync(directory, { withFileTypes: true })) {
    const path = join(directory, entry.name);
    if (entry.isDirectory()) {
      if (entry.name === 'node_modules' || entry.name === 'dist') {
        continue;
      }
      found.push(...sources(path));
      continue;
    }
    if (entry.name.includes('.test.')) {
      continue;
    }
    if (entry.name.endsWith('.tsx') || entry.name.endsWith('.ts')) {
      found.push(path);
    }
  }
  return found;
}

/**
 * The text of every `className=` value in one file.
 *
 * Both spellings: the attribute string, and the expression, whose braces are
 * counted so that a conditional or a template comes out whole.
 */
function classNameRegions(text: string): string[] {
  const regions: string[] = [];
  let at = text.indexOf('className=');
  while (at !== -1) {
    let cursor = at + 'className='.length;
    if (text[cursor] === '"') {
      const close = text.indexOf('"', cursor + 1);
      if (close !== -1) {
        regions.push(text.slice(cursor, close + 1));
      }
    } else if (text[cursor] === '{') {
      let depth = 0;
      const start = cursor;
      while (cursor < text.length) {
        const character = text[cursor];
        if (character === '{') {
          depth += 1;
        } else if (character === '}') {
          depth -= 1;
          if (depth === 0) {
            break;
          }
        }
        cursor += 1;
      }
      regions.push(text.slice(start, cursor + 1));
    }
    at = text.indexOf('className=', at + 1);
  }
  return regions;
}

/** What one region names, and how much of it could not be resolved. */
function classesIn(region: string): { readonly names: string[]; readonly unresolved: number } {
  const names: string[] = [];
  let unresolved = 0;

  // Quoted strings, including the literal halves of a template. Splitting a
  // template on its interpolations is what lets `clipped-btn` be read out of
  // `` `clipped-btn ${…}` `` while the interpolated half is counted instead.
  const quoted = region.matchAll(/'([^']*)'|"([^"]*)"|`([^`]*)`/g);
  for (const match of quoted) {
    const literal = match[1] ?? match[2] ?? match[3] ?? '';
    const template = match[3] !== undefined;
    if (!template) {
      names.push(...literal.split(/\s+/).filter((token) => token.startsWith('clipped-')));
      continue;
    }
    // A chunk that ends without whitespace before an interpolation is a class
    // the interpolation *extends*, so the fragment is not a class of its own.
    const chunks = literal.split(/\$\{[^}]*\}/);
    chunks.forEach((chunk, index) => {
      const extended = index < chunks.length - 1 && !/\s$/.test(chunk);
      const tokens = chunk.split(/\s+/).filter((token) => token.startsWith('clipped-'));
      if (extended && tokens.length > 0) {
        unresolved += 1;
        tokens.pop();
      }
      names.push(...tokens);
    });
  }

  return { names, unresolved };
}

describe('the window’s styles', () => {
  it('names only classes the stylesheet defines', () => {
    const files = MARKUP.flatMap((directory) => sources(directory));
    expect(
      files.length,
      'the window’s source was not found, so this test read nothing',
    ).toBeGreaterThan(20);

    const used = new Set<string>();
    let unresolved = 0;
    for (const file of files) {
      const text = readFileSync(file, 'utf8');
      for (const region of classNameRegions(text)) {
        const found = classesIn(region);
        found.names.forEach((name) => used.add(name));
        unresolved += found.unresolved;
      }
    }
    expect(used.size, 'no classes were extracted, so this test compared nothing').toBeGreaterThan(
      50,
    );

    const defined = new Set<string>();
    for (const sheet of readdirSync(STYLESHEETS).filter((name) => name.endsWith('.css'))) {
      const text = readFileSync(join(STYLESHEETS, sheet), 'utf8');
      for (const match of text.matchAll(/\.(clipped-[a-zA-Z0-9_-]+)/g)) {
        defined.add(match[1] as string);
      }
    }
    expect(defined.size, 'no stylesheet was read').toBeGreaterThan(50);

    const missing = [...used].filter((name) => !defined.has(name)).sort();
    expect(
      missing,
      'these classes are named by the window and defined by no stylesheet, so whatever wears ' +
        'them renders unstyled and no other check will say so (issue #688)',
    ).toEqual([]);

    expect(
      unresolved,
      'a class built at runtime cannot be checked from the markup. That is allowed and the ' +
        'count is asserted, so that adding one is a decision rather than a silent loss of ' +
        'coverage — update this number and check the spellings by hand',
    ).toBe(BUILT_AT_RUNTIME);
  });
});
