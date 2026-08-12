/**
 * Catching a figure nobody measured.
 *
 * The tempting failure on both library screens is the dense-looking one: tiles
 * reading "217 sessions", "48 clips", "16 favourites", "83 GB" — or the same
 * tiles reading zero. Nothing in this build has counted any of those, so either
 * is invented (AGENTS.md section 27), and a zeroed tile is additionally
 * indistinguishable from the true answer on a machine that has recorded nothing.
 *
 * Shared by the Home and Library cases because it is one property, checked the
 * same way on two screens.
 */

/**
 * A figure against one of the nouns SPEC.md sections 17, 29 and 30 count.
 *
 * Any figure, not only zero: the screen has measured nothing, so a number
 * against any of these words is a number somebody made up.
 */
export const A_COUNT =
  /\b\d[\d,.]*\s*(?:sessions?|recordings?|clips?|favourites?|highlights?|games?|[KMGT]?B)\b/i;

/**
 * Every run of text under an element, each on its own.
 *
 * Deliberately not `element.textContent`, which concatenates adjacent cells with
 * nothing between them: a row ending "Issue #301" beside one beginning "Clips"
 * reads as "301Clips", which fires the pattern above on a screen that draws no
 * figure at all — and, the other way round, a real "0 sessions" followed by the
 * next heading reads as "sessionsWhat", whose missing word boundary made the
 * pattern *miss* the very thing it exists for. Both of those happened while this
 * check was being written, in the two directions that matter.
 */
export function textRuns(element: Element): readonly string[] {
  const runs: string[] = [];
  const walker = element.ownerDocument.createTreeWalker(element, NodeFilter.SHOW_TEXT);
  for (let node = walker.nextNode(); node !== null; node = walker.nextNode()) {
    const text = node.textContent?.trim() ?? '';
    if (text !== '') {
      runs.push(text);
    }
  }
  return runs;
}
