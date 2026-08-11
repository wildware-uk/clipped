// @vitest-environment node
//
// Holds the stylesheets to the three rules issue #79 asks for and that a review
// cannot enforce: no colour, typeface, distance or leading is written as a value
// outside `tokens.css`; every interactive component has a keyboard focus ring;
// and every control that can be disabled is drawn as disabled.
//
// A promise in a comment that "everything names a token" was true when it was
// written. The check below is true whenever it passes, which is the difference
// that matters: adding `color: #fff` to a component, or pointing a rule at a
// token nobody declared, fails the suite rather than shipping.
//
// The rule this file learned the hard way is that a check must assert what the
// stylesheet *draws*, not that a string appears in it. Its focus-ring check
// used to read `expect(selectors).toContain(selector)`: deleting the `outline`
// declaration from a rule while leaving the selector behind kept the suite
// green, and two of the four selectors it listed declared no outline at all, so
// it was passing over components that did not do what it claimed. Everything
// below goes through `bodyOf`, which throws when the rule is missing, and
// asserts a declaration rather than a name.
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

/**
 * Every unit CSS measures a distance in, so that the gate is the claim rather
 * than a sample of it.
 *
 * A review of this file caught it covering only `px`, `rem` and `em`, and only
 * in lower case: `12PX`, `0.5EM`, `4pt`, `3VW` and `62ch` all passed, and
 * `styles.css` was in fact shipping `max-width: 62ch`. A gate narrower than the
 * claim built on it is worse than a narrower claim, because the claim is what
 * gets believed.
 *
 * Percentages and `fr` are deliberately absent. Both are proportions of
 * something else rather than distances — `width: 100%`, `1fr` — so there is
 * nothing for a token to hold.
 */
const LENGTH_UNITS = [
  /* absolute */
  'px',
  'cm',
  'mm',
  'q',
  'in',
  'pt',
  'pc',
  /* font-relative, and their root-relative twins */
  'em',
  'rem',
  'ex',
  'rex',
  'ch',
  'rch',
  'cap',
  'rcap',
  'ic',
  'ric',
  'lh',
  'rlh',
  /* viewport-relative, in all four viewport flavours */
  ...['', 's', 'l', 'd'].flatMap((flavour) =>
    ['vw', 'vh', 'vi', 'vb', 'vmin', 'vmax'].map((axis) => `${flavour}${axis}`),
  ),
] as const;

describe('the stylesheets', () => {
  it('load the component layer, so it reaches the window', () => {
    expect(read('styles.css')).toContain("@import './components.css';");
  });

  /*
   * The literal forms `tokens.css` carries, and which therefore may not appear
   * anywhere else. Each is a form the design system has a token for, so a
   * literal is always a component that stopped consuming the system.
   *
   * Every pattern is case-insensitive. CSS is, so a gate that is not lets the
   * same value through in capitals.
   */
  const LITERALS: readonly (readonly [string, RegExp])[] = [
    ['hex colours', /#[0-9a-f]{3,8}\b/i],
    ['colour functions', /\b(?:rgba?|hsla?|hwb|lab|lch|oklab|oklch)\s*\(/i],
    [
      'distances',
      new RegExp(String.raw`(?<![\w-])-?\d*\.?\d+(?:${LENGTH_UNITS.join('|')})\b`, 'i'),
    ],
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

    /*
     * Leading is the one design value with no unit to catch it: `1.55` is a
     * ratio, so the distances pattern above cannot see it, and four of them
     * were sitting in these two files under a comment promising no literals.
     * It gets its own check for the same reason `font-family` does.
     */
    it('names a token for every leading it sets', () => {
      const leadings = [...declarations(sheet).matchAll(/line-height:\s*([^;]+);/g)].map((found) =>
        (found[1] ?? '').trim(),
      );

      expect(leadings.length).toBeGreaterThan(0);
      for (const leading of leadings) {
        expect(leading).toMatch(/^var\(--leading-[\w-]+\)$/);
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

/**
 * The declarations of the rule with exactly this selector, or a failure naming
 * the selector that is missing.
 *
 * Every check below that says "this component draws X" goes through here, so
 * that deleting the rule and emptying it both fail. Asking whether a selector
 * appears somewhere in the package — which is what the focus-ring check used to
 * do — answers a question nobody was asking.
 */
function bodyOf(selector: string): string {
  const found = ALL_RULES.find(([, candidate]) => candidate === selector);
  if (!found) {
    throw new Error(`no "${selector}" rule in ${COMPONENT_SHEETS.join(' or ')}`);
  }
  return found[2];
}

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
   * The components that draw their own ring, because the global rule cannot
   * reach them: a radio and a segmented option both keep a real `<input>`
   * off-screen for the keyboard behaviour and paint a stand-in beside it, so
   * `:focus-visible` never matches the element that is drawn.
   *
   * This asserts the *declaration*, not the selector. A review found the check
   * it replaces reading `expect(selectors).toContain(selector)` — deleting the
   * `outline` from the radio's rule while leaving the selector and its
   * `outline-offset` behind left the whole suite green, and two of the four
   * selectors it listed declared no outline at all, so the check was passing
   * over components that did not do what it claimed.
   */
  const OWN_RING: readonly string[] = [
    '.clipped-radio input:focus-visible + .clipped-radio__dot',
    '.clipped-segment__option:has(input:focus-visible)',
  ];

  it.each(OWN_RING)('is declared by %s, whose real input is off-screen', (selector) => {
    expect(bodyOf(selector)).toMatch(
      /(^|\n)\s*outline:\s*var\(--rule-weight\) solid var\(--color-accent\);/,
    );
  });

  /*
   * The components that take the global ring and only move it. Neither declares
   * an outline, and neither should: a field pulls the ring flush against its
   * border box so that in a dense form it does not collide with the field
   * above, and a navigation link pulls it inside because a link spans the
   * sidebar's full width and an outward ring would be clipped against the
   * divider on its right.
   *
   * Asserting that they do *not* restate the outline is the point. The two were
   * previously listed as drawing their own ring and documented as replacing it,
   * which was true of neither.
   */
  const MOVED_RING: readonly string[] = [
    '.clipped-input:focus-visible',
    '.clipped-nav__link:focus-visible',
  ];

  it.each(MOVED_RING)('is the global ring moved by %s, not a second one', (selector) => {
    const body = bodyOf(selector);

    expect(body).toMatch(/(^|\n)\s*outline-offset:\s*[^;]+;/);
    expect(body).not.toMatch(/(^|\n)\s*outline:/);
  });

  /*
   * The segmented control clips its children, which is why its ring is drawn
   * inward — and inward means the ring lands on the option's own fill, which on
   * the selected option is `--color-accent-solid`. These two facts have to move
   * together: an outward offset with the clip still in place is a ring cut off
   * on the first and last option, and `contrast.test.ts` measures the ring
   * against the fill it is drawn over rather than against the window ground.
   */
  it('keeps the segmented ring inside the clip that would otherwise cut it off', () => {
    expect(bodyOf('.clipped-segment')).toMatch(/(^|\n)\s*overflow:\s*hidden;/);
    expect(bodyOf('.clipped-segment__option:has(input:focus-visible)')).toMatch(
      /(^|\n)\s*outline-offset:\s*calc\(var\(--rule-weight\) \* -1\);/,
    );
  });
});

/*
 * Issue #79 asks for "disabled at reduced opacity" of the component set as a
 * whole. Every control that can be disabled is listed, because a review found
 * three of the four had no rule at all: a disabled field, radio or segmented
 * option was drawn identically to a live one, and nothing said so.
 */
describe('a disabled control', () => {
  const DISABLED: readonly string[] = [
    '.clipped-btn:disabled',
    '.clipped-input:disabled',
    '.clipped-radio:has(input:disabled)',
    '.clipped-segment__option:has(input:disabled)',
  ];

  it.each(DISABLED)('is dimmed and refuses the pointer, for %s', (selector) => {
    const body = bodyOf(selector);

    expect(body).toMatch(/(^|\n)\s*opacity:\s*var\(--disabled-opacity\);/);
    expect(body).toMatch(/(^|\n)\s*cursor:\s*not-allowed;/);
  });
});
