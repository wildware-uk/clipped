// @vitest-environment node
//
// Measures the shell and the component layer against WCAG 2.1's contrast
// minima: 1.4.3 for text, and 1.4.11 for the edges and rings that identify a
// control without being text.
//
// Issue #48 asks for sufficient contrast and AGENTS.md section 46 repeats it,
// and a comment in `tokens.css` saying a pair "measures about 6.4:1" is not a
// check - it is a claim that was true once. This file computes the ratios from
// the token values and from the rules that use them, so retuning a token to
// something too pale, or pointing a rule at the wrong one, fails the suite.
//
// Every case below names a *rule and a property*, never a pair of tokens. That
// is the difference between measuring the stylesheet and measuring two
// constants: a case written as `['var(--color-text-muted)', 'var(--color-bg)']`
// goes on passing after the rule it claims to be about has been pointed at
// something else entirely. Issue #48 shipped that defect in the skip link, and
// a review of this file found it again in fourteen of its own cases, so the
// mechanism the skip link already used is now the only one there is.
//
// Every foreground here is held to body text's 4.5:1 rather than large text's
// 3:1. The shell's biggest words are `--text-3xl`, 26px at weight 800, which
// does clear WCAG's 18.66px-bold threshold for large text - but the ratio that
// matters is the one every other string is held to, and letting one heading off
// buys nothing.
//
// It runs in the node environment because nothing here renders and the subject
// is the stylesheets as text: node is what gives `import.meta.url` a real file
// URL to read them through, and Vitest replaces a CSS import with an empty
// module, so a `?raw` import would hand this file an empty string and every
// assertion below would pass on nothing.

import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

import { describe, expect, it } from 'vitest';

const here = fileURLToPath(new URL('.', import.meta.url));

/** A stylesheet as text. Whether the working copy carries CRLF is a git
 * setting rather than a fact about the file, so it is normalised away. */
function read(name: string): string {
  return readFileSync(`${here}${name}`, 'utf8').replace(/\r\n/g, '\n');
}

const tokensCss = read('tokens.css');
const stylesCss = read('styles.css');
const componentsCss = read('components.css');

const AA_NORMAL_TEXT = 4.5;

/**
 * WCAG 2.1 1.4.11, the minimum for anything that is not text but still has to
 * be seen: the edge that says "this is a text field", and the focus ring.
 */
const AA_NON_TEXT = 3;

/** A colour as sRGB channels in 0..255, with the alpha it is painted at. */
interface Colour {
  readonly rgb: readonly [number, number, number];
  readonly alpha: number;
}

/**
 * A capture group that must have matched, or a failure naming what did not.
 *
 * A pattern below that stops matching means this file has quietly stopped
 * measuring the stylesheet, which is worse than it failing, so every group is
 * taken through here rather than through a non-null assertion.
 */
function group(match: RegExpExecArray | RegExpMatchArray, index: number, what: string): string {
  const captured = match[index];
  if (captured === undefined) {
    throw new Error(`could not read ${what} out of "${match[0]}"`);
  }
  return captured;
}

/** The custom properties declared in `tokens.css`'s `:root` block. */
function readTokens(css: string): ReadonlyMap<string, string> {
  const root = /:root\s*\{([\s\S]*)\n\}/.exec(css);
  if (!root) {
    throw new Error('tokens.css has no :root block');
  }

  const tokens = new Map<string, string>();
  for (const declared of group(root, 1, 'the :root block').matchAll(/(--[\w-]+):\s*([^;]+);/g)) {
    tokens.set(group(declared, 1, 'a token name'), group(declared, 2, 'a token value').trim());
  }
  return tokens;
}

const TOKENS = readTokens(tokensCss);

/**
 * Resolves a token value to a colour.
 *
 * Only the three forms `tokens.css` actually uses are understood — a `var()`
 * reference, a six-digit hex, and a `color-mix()` towards `transparent`, which
 * is how the file expresses "this ink at N% opacity". Anything else throws
 * rather than being guessed at, because a colour this file cannot read is a
 * colour it would otherwise silently stop measuring.
 */
function resolve(value: string): Colour {
  const reference = /^var\((--[\w-]+)\)$/.exec(value.trim());
  if (reference) {
    const name = group(reference, 1, 'a token reference');
    const referenced = TOKENS.get(name);
    if (referenced === undefined) {
      throw new Error(`${name} is not declared in tokens.css`);
    }
    return resolve(referenced);
  }

  const hex = /^#([0-9a-f]{6})$/i.exec(value.trim());
  if (hex) {
    const channels = group(hex, 1, 'the channels of a hex colour');
    return {
      rgb: [
        Number.parseInt(channels.slice(0, 2), 16),
        Number.parseInt(channels.slice(2, 4), 16),
        Number.parseInt(channels.slice(4, 6), 16),
      ],
      alpha: 1,
    };
  }

  const mix = /^color-mix\(\s*in srgb\s*,\s*(\S+)\s+([\d.]+)%\s*,\s*transparent\s*\)$/.exec(
    value.trim(),
  );
  if (mix) {
    return {
      rgb: resolve(group(mix, 1, 'the colour being mixed')).rgb,
      alpha: Number.parseFloat(group(mix, 2, 'the percentage it is mixed at')) / 100,
    };
  }

  throw new Error(`cannot read the colour "${value}"`);
}

/** The colour actually seen when `foreground` is painted over `background`. */
function composite(foreground: Colour, background: Colour): readonly [number, number, number] {
  const blend = (over: number, under: number): number =>
    over * foreground.alpha + under * (1 - foreground.alpha);
  return [
    blend(foreground.rgb[0], background.rgb[0]),
    blend(foreground.rgb[1], background.rgb[1]),
    blend(foreground.rgb[2], background.rgb[2]),
  ];
}

/** WCAG 2.1 relative luminance, from sRGB channels in 0..255. */
function relativeLuminance(rgb: readonly [number, number, number]): number {
  const linear = (channel: number): number => {
    const scaled = channel / 255;
    return scaled <= 0.03928 ? scaled / 12.92 : ((scaled + 0.055) / 1.055) ** 2.4;
  };
  return 0.2126 * linear(rgb[0]) + 0.7152 * linear(rgb[1]) + 0.0722 * linear(rgb[2]);
}

/**
 * WCAG 2.1 contrast ratio, `(L1 + 0.05) / (L2 + 0.05)`.
 *
 * The background must be opaque: everything the shell draws words on is either
 * a solid token or the window's own ground, and a translucent background would
 * need whatever is behind *it* to be meaningful.
 */
function contrastRatio(foreground: string, background: string): number {
  const ground = resolve(background);
  expect(ground.alpha, `${background} is translucent and cannot be a background`).toBe(1);

  const ink = relativeLuminance(composite(resolve(foreground), ground));
  const paper = relativeLuminance(ground.rgb);
  return (Math.max(ink, paper) + 0.05) / (Math.min(ink, paper) + 0.05);
}

/** One declaration from the first rule matching `selector` in a stylesheet. */
function declaration(css: string, selector: string, property: string): string {
  const rule = new RegExp(`(^|\\n)${selector}\\s*\\{([^}]*)\\}`).exec(css);
  if (!rule) {
    throw new Error(`no "${selector}" rule`);
  }

  const body = group(rule, 2, `the body of the "${selector}" rule`);
  const found = new RegExp(`(^|\\n)\\s*${property}:\\s*([^;]+);`).exec(body);
  if (!found) {
    throw new Error(`the "${selector}" rule declares no ${property}`);
  }
  return group(found, 2, `${property} in the "${selector}" rule`).trim();
}

/**
 * The top-level, whitespace-separated components of a value, with anything
 * inside parentheses left whole.
 *
 * `var(--rule-weight) solid var(--color-accent)` is three parts;
 * `color-mix(in srgb, var(--color-text) 40%, transparent)` is one.
 */
function parts(value: string): readonly string[] {
  const found: string[] = [];
  let depth = 0;
  let current = '';

  for (const character of value.trim()) {
    if (character === '(') {
      depth += 1;
    } else if (character === ')') {
      depth -= 1;
    }

    if (depth === 0 && /\s/.test(character)) {
      if (current !== '') {
        found.push(current);
        current = '';
      }
      continue;
    }
    current += character;
  }

  if (current !== '') {
    found.push(current);
  }
  return found;
}

/**
 * The sheets a rule can be read from, so that a case below names one rather
 * than closing over a string and being unreadable in the table.
 */
const SHEETS = {
  styles: stylesCss,
  components: componentsCss,
} as const;

/**
 * A colour, named by the rule that paints it: which sheet, which selector, and
 * which property. The selector is a regular expression source, because a CSS
 * selector is full of characters a pattern reads — hence the escaping in the
 * tables below.
 *
 * This is the whole point of the file. A case that named a pair of tokens by
 * hand would measure two constants and go on passing while the rule it claims
 * to be about was pointed somewhere else; issue #48 shipped exactly that defect
 * in the skip link, and a review of this file found it again in fourteen of its
 * own cases. Every case therefore resolves its colour out of the stylesheet, so
 * that changing a rule changes what is measured.
 */
type Painted = readonly [sheet: keyof typeof SHEETS, selector: string, property: string];

/**
 * The colour a rule paints for one property, as a value `resolve` understands.
 *
 * A colour is either the whole value — `color`, `background`, `border-color` —
 * or the last component of a shorthand: `border` and `outline` are
 * `<width> <style> <colour>`, and a `box-shadow` ends in its colour too. Taking
 * the last part and letting `resolve` reject anything that is not a colour
 * means a rule that stops declaring one fails rather than being skipped.
 */
function colourOf([sheet, selector, property]: Painted): string {
  const value = declaration(SHEETS[sheet], selector, property);
  const components = parts(value);
  const last = components[components.length - 1];
  if (last === undefined) {
    throw new Error(`${property} in the "${selector}" rule is empty`);
  }
  return last;
}

/** The measured ratio between two colours, each read out of its own rule. */
function ratioBetween(ink: Painted, ground: Painted): number {
  return contrastRatio(colourOf(ink), colourOf(ground));
}

/* The grounds the interface draws on, each read from the rule that fills it. */
const WINDOW: Painted = ['styles', 'body', 'background'];
const SIDEBAR: Painted = ['styles', '\\.clipped-sidebar', 'background'];
const TITLE_STRIP: Painted = ['styles', '\\.clipped-header', 'background'];
const CARD: Painted = ['components', '\\.clipped-card', 'background'];
const DIALOG: Painted = ['components', '\\.clipped-dialog', 'background'];
const FIELD: Painted = ['components', '\\.clipped-input', 'background'];

/* The ink the window inherits where a component sets none of its own. */
const INHERITED: Painted = ['styles', 'body', 'color'];

/* The global focus ring, which most components take rather than declare. */
const GLOBAL_RING: Painted = ['styles', ':focus-visible', 'outline'];

describe('the shell', () => {
  /*
   * The skip link is read out of the stylesheet rather than named here, because
   * the defect this test exists for was the rule pointing at `--color-accent`,
   * whose 3.76:1 fails. A hard-coded pair would have gone on passing.
   */
  it('draws the skip link, the first control a keyboard reaches, at 4.5:1 or better', () => {
    const ratio = ratioBetween(
      ['styles', '\\.clipped-skip-link', 'color'],
      ['styles', '\\.clipped-skip-link', 'background'],
    );

    expect(ratio).toBeGreaterThanOrEqual(AA_NORMAL_TEXT);
  });

  /*
   * Every pairing of words and ground the shell actually draws. Which pairs
   * meet is by hand, because the cascade cannot be computed without a browser —
   * but both colours are read out of the rules that paint them, so retuning a
   * token or repointing a rule changes what is measured here.
   */
  const PAIRINGS: readonly (readonly [string, Painted, Painted])[] = [
    ['body text on the window ground', INHERITED, WINDOW],
    ['secondary text on the window ground', ['styles', '\\.clipped-muted', 'color'], WINDOW],
    ['secondary text in the sidebar', ['styles', '\\.clipped-status__detail', 'color'], SIDEBAR],
    ['a link on the window ground', ['styles', 'a', 'color'], WINDOW],
    [
      'the open navigation item',
      ['styles', "\\.clipped-nav__link\\[aria-current='page'\\]", 'color'],
      SIDEBAR,
    ],
    ['the title strip', ['styles', '\\.clipped-header', 'color'], TITLE_STRIP],
    ['the title strip tagline', ['styles', '\\.clipped-header__tagline', 'color'], TITLE_STRIP],
  ];

  it.each(PAIRINGS)('draws %s at 4.5:1 or better', (_name, ink, ground) => {
    expect(ratioBetween(ink, ground)).toBeGreaterThanOrEqual(AA_NORMAL_TEXT);
  });
});

/* The two rules of the segmented control that the focus ring is measured on. */
const SEGMENT_RING: string = '\\.clipped-segment__option:has\\(input:focus-visible\\)';
const SEGMENT_SELECTED: string = '\\.clipped-segment__option:has\\(input:checked\\)';

describe('the component layer', () => {
  /*
   * Every pairing of words and ground the component layer draws, each colour
   * read out of the rule that paints it. A hovered or pressed state declares
   * only a ground, so its ink comes from the rule it modifies; a component that
   * sets no colour of its own inherits the window's, which is `INHERITED`.
   *
   * Two grounds are new here and neither existed when issue #48 measured the
   * shell: `--color-surface`, which cards, fields and the dialog are filled
   * with, and the 100 steps of the two ramps, which the tags are tinted from.
   */
  const PAIRINGS: readonly (readonly [string, Painted, Painted])[] = [
    [
      'the primary button',
      ['components', '\\.clipped-btn--primary', 'color'],
      ['components', '\\.clipped-btn--primary', 'background'],
    ],
    [
      'the primary button hovered',
      ['components', '\\.clipped-btn--primary', 'color'],
      ['components', '\\.clipped-btn--primary:hover', 'background'],
    ],
    [
      'the primary button pressed',
      ['components', '\\.clipped-btn--primary', 'color'],
      ['components', '\\.clipped-btn--primary:active', 'background'],
    ],
    [
      'the selected segment',
      ['components', SEGMENT_SELECTED, 'color'],
      ['components', SEGMENT_SELECTED, 'background'],
    ],
    ["a button's label on the window ground", ['components', '\\.clipped-btn', 'color'], WINDOW],
    ['the ghost button', ['components', '\\.clipped-btn--ghost', 'color'], WINDOW],
    [
      'the accent tag',
      ['components', '\\.clipped-tag--accent', 'color'],
      ['components', '\\.clipped-tag--accent', 'background'],
    ],
    [
      'the neutral tag',
      ['components', '\\.clipped-tag--neutral', 'color'],
      ['components', '\\.clipped-tag--neutral', 'background'],
    ],
    ['the outlined tag', ['components', '\\.clipped-tag--outline', 'color'], WINDOW],
    ["a field's label", ['components', '\\.clipped-field__label', 'color'], WINDOW],
    ["a field's own text", ['components', '\\.clipped-input', 'color'], FIELD],
    ["a table's header", ['components', '\\.clipped-table th', 'color'], WINDOW],
    ['body text on a card', INHERITED, CARD],
    ["a card's kicker", ['components', '\\.clipped-card__kicker', 'color'], CARD],
    ["a card's body", ['components', '\\.clipped-card__body', 'color'], CARD],
    ["a card's meta row", ['components', '\\.clipped-card__meta', 'color'], CARD],
    ["a dialog's title", INHERITED, DIALOG],
    ["a dialog's body", ['components', '\\.clipped-dialog__body', 'color'], DIALOG],
  ];

  it.each(PAIRINGS)('draws %s at 4.5:1 or better', (_name, ink, ground) => {
    expect(ratioBetween(ink, ground)).toBeGreaterThanOrEqual(AA_NORMAL_TEXT);
  });

  /*
   * What is not text but still has to be seen. The edge is what tells someone
   * a field is a field, and the ring is what tells them where the keyboard is;
   * WCAG 2.1 1.4.11 puts both at 3:1. `--color-divider`, which the design
   * system draws the edge in, measures 2.41:1 on the window ground - which is
   * the whole reason `--color-control-edge` exists.
   *
   * The rules *between* things - `.clipped-rule`, the table's row rules, the
   * sidebar's dividers - are deliberately not here. 1.4.11 applies to what
   * identifies a control or conveys information, and a rule separating two
   * paragraphs does neither; that is why `--color-divider` is allowed to stay
   * at 2.41:1 for those and nothing else.
   */
  const NON_TEXT: readonly (readonly [string, Painted, Painted])[] = [
    ["a field's edge on the window ground", ['components', '\\.clipped-input', 'border'], WINDOW],
    ["a field's edge against its own fill", ['components', '\\.clipped-input', 'border'], FIELD],
    ["a field's edge on a card", ['components', '\\.clipped-input', 'border'], CARD],
    [
      "a secondary button's edge",
      ['components', '\\.clipped-btn--secondary', 'border-color'],
      WINDOW,
    ],
    ["a radio's ring", ['components', '\\.clipped-radio__dot', 'border'], WINDOW],
    ["the segmented control's edge", ['components', '\\.clipped-segment', 'border'], WINDOW],
    ['the focus ring on the window ground', GLOBAL_RING, WINDOW],
    ['the focus ring on a card or a dialog', GLOBAL_RING, CARD],
    ['the focus ring in the sidebar', GLOBAL_RING, SIDEBAR],
    [
      "a focused field's accent border against its own fill",
      ['components', '\\.clipped-input:focus-visible', 'border-color'],
      FIELD,
    ],
    [
      "the radio's own ring, on the stand-in the global rule cannot reach",
      ['components', '\\.clipped-radio input:focus-visible \\+ \\.clipped-radio__dot', 'outline'],
      WINDOW,
    ],
    /*
     * The segmented option's ring is drawn *inside* its own border box, because
     * the control clips its children - so on the selected option it lands on
     * `--color-accent-solid`, where the accent measures 1.71:1. That case was
     * missing from this list while the three grounds it happens to pass on were
     * in it, which is precisely the option a keyboard user reaches first. The
     * ring therefore carries a halo of the window ground just inside it, and
     * both of its edges are measured: the accent against the halo, and the halo
     * against the fill it is drawn on. Dropping the halo makes `colourOf` throw
     * on a rule that declares no `box-shadow`, rather than quietly passing.
     */
    [
      "the segmented option's ring against its own halo",
      ['components', SEGMENT_RING, 'outline'],
      ['components', SEGMENT_RING, 'box-shadow'],
    ],
    [
      "the segmented option's halo on the selected option",
      ['components', SEGMENT_RING, 'box-shadow'],
      ['components', SEGMENT_SELECTED, 'background'],
    ],
  ];

  it.each(NON_TEXT)('draws %s at 3:1 or better', (_name, ink, ground) => {
    expect(ratioBetween(ink, ground)).toBeGreaterThanOrEqual(AA_NON_TEXT);
  });
});
