// @vitest-environment node
//
// Holds the stylesheets to the two rules issue #79 asks for and that a review
// cannot enforce: no colour, typeface or distance is written as a value outside
// `tokens.css`, and every interactive component draws a keyboard focus ring.
//
// A promise in a comment that "everything names a token" was true when it was
// written. The check below is true whenever it passes, which is the difference
// that matters: adding `color: #fff` to a component, or pointing a rule at a
// token nobody declared, fails the suite rather than shipping.
//
// It runs in the node environment for the same reason `contrast.test.ts` does:
// the subject is the stylesheets as text, and Vitest replaces a CSS import with
// an empty module, so a `?raw` import would hand this file an empty string and
// every assertion below would pass on nothing.

import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

import { describe, expect, it } from 'vitest';

const here = fileURLToPath(new URL('.', import.meta.url));

/** A stylesheet as text, with CRLF - a git setting, not a fact about the file -
 * normalised away. */
function read(name: string): string {
  return readFileSync(`${here}${name}`, 'utf8').replace(/\r\n/g, '\n');
}

/**
 * The declarations of a stylesheet, with its comments removed.
 *
 * Comments are stripped because they discuss the values they exist to explain —
 * "`--color-accent` measures 3.72:1", "14px at weight 800" — and a check that
 * fired on prose would push the explanations out of the file.
 */
function declarations(name: string): string {
  return read(name).replace(/\/\*[\s\S]*?\*\//g, '');
}

const TOKENS_CSS = read('tokens.css');

/** Every stylesheet in this package that is not the token sheet. */
const COMPONENT_SHEETS = ['styles.css', 'components.css'] as const;

describe('the stylesheets', () => {
  it('load the component layer, so it reaches the window', () => {
    expect(read('styles.css')).toContain("@import './components.css';");
  });

  /*
   * The literal forms `tokens.css` carries, and which therefore may not appear
   * anywhere else. Each is a form the design system has a token for, so a
   * literal is always a component that stopped consuming the system.
   */
  const LITERALS: readonly (readonly [string, RegExp])[] = [
    ['hex colours', /#[0-9a-f]{3,8}\b/i],
    ['colour functions', /\b(?:rgba?|hsla?|hwb|lab|lch|oklab|oklch)\s*\(/i],
    ['pixel values', /(?<![\w-])-?\d*\.?\d+px\b/],
    ['rem or em values', /(?<![\w-])-?\d*\.?\d+r?em\b/],
  ];

  describe.each(COMPONENT_SHEETS)('%s', (sheet) => {
    it.each(LITERALS)('writes no %s', (_what, pattern) => {
      expect(declarations(sheet)).not.toMatch(pattern);
    });

    it('names a token for every typeface it sets', () => {
      const families = [...declarations(sheet).matchAll(/font-family:\s*([^;]+);/g)].map((found) =>
        (found[1] ?? '').trim(),
      );

      expect(families.length).toBeGreaterThan(0);
      for (const family of families) {
        expect(family).toMatch(/^var\(--font-[\w-]+\)$/);
      }
    });
  });

  /*
   * A `var()` naming a token nobody declares resolves to nothing, and the
   * property silently falls back to its initial value — a colour becomes black,
   * a padding becomes zero. Nothing about the page says so, which is why this
   * is checked here rather than left to be noticed.
   */
  it('reference only tokens that tokens.css declares', () => {
    const declared = new Set(
      [...TOKENS_CSS.matchAll(/^\s*(--[\w-]+):/gm)].map((found) => found[1] ?? ''),
    );
    expect(declared.size).toBeGreaterThan(0);

    for (const sheet of [...COMPONENT_SHEETS, 'tokens.css']) {
      for (const use of declarations(sheet).matchAll(/var\(\s*(--[\w-]+)/g)) {
        expect(declared, `${sheet} reads ${use[1] ?? ''}`).toContain(use[1] ?? '');
      }
    }
  });
});

/** Every `selector { body }` pair in a stylesheet, comments removed. */
function rules(name: string): readonly (readonly [string, string])[] {
  return [...declarations(name).matchAll(/([^{}]+)\{([^{}]*)\}/g)].map(
    (rule) => [(rule[1] ?? '').trim(), rule[2] ?? ''] as const,
  );
}

const ALL_RULES = COMPONENT_SHEETS.flatMap((sheet) =>
  rules(sheet).map((rule) => [sheet, ...rule] as const),
);

describe('keyboard focus', () => {
  it('is drawn as the accent outline the design system specifies', () => {
    const global = rules('styles.css').find(([selector]) => selector === ':focus-visible');

    expect(global?.[1]).toContain('outline: var(--rule-weight) solid var(--color-accent);');
    expect(global?.[1]).toContain('outline-offset: var(--rule-weight);');
  });

  /*
   * `:focus { outline: none }` is what removes the browser's own ring, and it
   * is the only place that may. A component that turned its ring off would take
   * itself out of the keyboard's reach while still looking finished.
   */
  it('is suppressed only for the pointer, never for the keyboard', () => {
    const suppressed = ALL_RULES.filter(([, , body]) => /outline:\s*none/.test(body)).map(
      ([sheet, selector]) => `${sheet} ${selector}`,
    );

    expect(suppressed).toEqual(['styles.css :focus']);
  });

  it('is never drawn in something other than the accent', () => {
    for (const [sheet, selector, body] of ALL_RULES) {
      const outline = /(^|\n)\s*outline:\s*([^;]+);/.exec(body);
      if (!outline || selector === ':focus') {
        continue;
      }
      expect(outline[2], `${sheet} ${selector}`).toBe(
        'var(--rule-weight) solid var(--color-accent)',
      );
    }
  });

  /*
   * The components whose ring cannot come from the global rule: two draw it on
   * a stand-in because the real input is off-screen, and one has to draw it
   * inward because the element it belongs to is clipped or full-width. Each is
   * listed here so that removing one is a failing test rather than a control
   * that is only findable with a mouse.
   */
  const OWN_RING: readonly string[] = [
    '.clipped-input:focus-visible',
    '.clipped-radio input:focus-visible + .clipped-radio__dot',
    '.clipped-segment__option:has(input:focus-visible)',
    '.clipped-nav__link:focus-visible',
  ];

  it.each(OWN_RING)('is drawn for %s', (selector) => {
    expect(ALL_RULES.map(([, found]) => found)).toContain(selector);
  });
});
