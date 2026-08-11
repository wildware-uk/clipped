// @vitest-environment node
//
// Measures the shell's text against WCAG 2.1 contrast minimum 1.4.3.
//
// Issue #48 asks for sufficient contrast and AGENTS.md section 46 repeats it,
// and a comment in `tokens.css` saying a pair "measures about 6.4:1" is not a
// check - it is a claim that was true once. This file computes the ratios from
// the token values and from the rules that use them, so retuning a token to
// something too pale, or pointing a rule at the wrong one, fails the suite.
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

const AA_NORMAL_TEXT = 4.5;

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

/** One declaration from the first rule matching `selector` in `styles.css`. */
function declaration(selector: string, property: string): string {
  const rule = new RegExp(`(^|\\n)${selector}\\s*\\{([^}]*)\\}`).exec(stylesCss);
  if (!rule) {
    throw new Error(`styles.css has no "${selector}" rule`);
  }

  const body = group(rule, 2, `the body of the "${selector}" rule`);
  const found = new RegExp(`(^|\\n)\\s*${property}:\\s*([^;]+);`).exec(body);
  if (!found) {
    throw new Error(`the "${selector}" rule declares no ${property}`);
  }
  return group(found, 2, `${property} in the "${selector}" rule`).trim();
}

describe('the shell', () => {
  /*
   * The skip link is read out of the stylesheet rather than named here, because
   * the defect this test exists for was the rule pointing at `--color-accent`,
   * whose 3.76:1 fails. A hard-coded pair would have gone on passing.
   */
  it('draws the skip link, the first control a keyboard reaches, at 4.5:1 or better', () => {
    const ratio = contrastRatio(
      declaration('\\.clipped-skip-link', 'color'),
      declaration('\\.clipped-skip-link', 'background'),
    );

    expect(ratio).toBeGreaterThanOrEqual(AA_NORMAL_TEXT);
  });

  /*
   * Every pairing of words and ground the shell actually draws. The list is by
   * hand because the cascade cannot be computed without a browser, but the
   * colours are the real token values: a token retuned too pale fails here even
   * though no rule changed.
   */
  const PAIRINGS: readonly (readonly [string, string, string])[] = [
    ['body text on the window ground', 'var(--color-text)', 'var(--color-bg)'],
    ['secondary text on the window ground', 'var(--color-text-muted)', 'var(--color-bg)'],
    ['secondary text in the sidebar', 'var(--color-text-muted)', 'var(--color-neutral-200)'],
    ['accent text on the window ground', 'var(--color-accent-text)', 'var(--color-bg)'],
    ['the open navigation item', 'var(--color-accent-text)', 'var(--color-neutral-200)'],
    ['the title strip', 'var(--color-neutral-200)', 'var(--color-neutral-900)'],
    ['the title strip tagline', 'var(--color-neutral-500)', 'var(--color-neutral-900)'],
  ];

  it.each(PAIRINGS)('draws %s at 4.5:1 or better', (_name, foreground, background) => {
    expect(contrastRatio(foreground, background)).toBeGreaterThanOrEqual(AA_NORMAL_TEXT);
  });
});
