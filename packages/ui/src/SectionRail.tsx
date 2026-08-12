import { useRef, type KeyboardEvent, type ReactNode } from 'react';

import { railPanelId, railTabId } from './railIds';

/** One section of a screen, as the rail lists it. */
export interface RailSection {
  /** Stable identifier, independent of the label. Used in the element ids. */
  readonly id: string;
  /** What the rail shows, in sentence case, as the design deck writes it. */
  readonly label: string;
}

/** A screen's own vertical rail of sections. */
export interface SectionRailProps {
  /**
   * What the rail is, for assistive technology. The window already has two
   * named navigation lists in the sidebar, so a rail inside a screen has to say
   * which set of sections it moves between.
   */
  readonly label: string;
  /**
   * A name unique to this rail, used to build the element ids that tie a
   * section to its pane. `railTabId` and `railPanelId` are how a screen writes
   * the other end of them.
   */
  readonly name: string;
  /** The sections, in the order the rail lists them. */
  readonly sections: readonly RailSection[];
  /** The section on show. */
  readonly currentId: string;
  /** Called when a section is chosen, by pointer or by keyboard. */
  readonly onSelect: (section: RailSection) => void;
}

/**
 * The rail down the left of a screen that has more sections than fit on one.
 *
 * It is drawn in the shell's own `.clipped-nav__link`, because a rail entry and
 * a sidebar item are the same thing to look at and a second class differing only
 * in its name would be the parallel styling `docs/desktop-ui.md` exists to
 * prevent. What differs is the mechanism beneath: the sidebar's items are
 * anchors with addresses, and these are `role="tab"` buttons, because a section
 * of a screen has no address of its own — the router's routes come from
 * `SCREENS`, one per screen, and the window title and the sidebar's marked item
 * are both derived from an exact path match.
 *
 * That makes the keyboard contract the WAI-ARIA one for a tab list, and it is
 * written out here rather than left to Tab alone: one stop in the tab order,
 * the arrow keys moving between sections, Home and End reaching the ends. A rail
 * of five buttons each taking a tab stop would put four stops between the
 * sidebar and the pane that a keyboard user has to walk past every time.
 *
 * Selection follows focus, which is the pattern's own recommendation for panes
 * that are cheap to show: everything behind this rail is already rendered text.
 */
export function SectionRail({
  label,
  name,
  sections,
  currentId,
  onSelect,
}: SectionRailProps): ReactNode {
  const entries = useRef<(HTMLButtonElement | null)[]>([]);

  /** Selects the section `delta` away from `from`, wrapping, and follows it. */
  const move = (from: number, delta: number): void => {
    const count = sections.length;
    if (count === 0) {
      return;
    }

    const to = (from + delta + count) % count;
    const section = sections[to];
    if (section === undefined) {
      return;
    }

    onSelect(section);
    entries.current[to]?.focus();
  };

  const onKeyDown = (event: KeyboardEvent<HTMLButtonElement>, index: number): void => {
    switch (event.key) {
      case 'ArrowDown':
      case 'ArrowRight':
        move(index, 1);
        break;
      case 'ArrowUp':
      case 'ArrowLeft':
        move(index, -1);
        break;
      case 'Home':
        move(index, -index);
        break;
      case 'End':
        move(index, sections.length - 1 - index);
        break;
      default:
        // Everything else is the window's: a rail must not eat Tab, and it must
        // not eat the shortcut somebody pressed while it happened to have focus.
        return;
    }

    // Only for the keys above, and only once one of them has been acted on. The
    // arrows scroll a pane that is taller than the window, so preventing the
    // default unconditionally would take that away.
    event.preventDefault();
  };

  return (
    <div className="clipped-rail" role="tablist" aria-label={label} aria-orientation="vertical">
      {sections.map((section, index) => {
        const current = section.id === currentId;

        return (
          <button
            key={section.id}
            ref={(element) => {
              entries.current[index] = element;
            }}
            type="button"
            role="tab"
            id={railTabId(name, section.id)}
            className="clipped-nav__link"
            aria-selected={current}
            aria-controls={railPanelId(name, section.id)}
            // One stop in the tab order, on the section that is open: the rest
            // are reached with the arrow keys.
            tabIndex={current ? 0 : -1}
            onClick={() => {
              onSelect(section);
            }}
            onKeyDown={(event) => {
              onKeyDown(event, index);
            }}
          >
            {section.label}
          </button>
        );
      })}
    </div>
  );
}
