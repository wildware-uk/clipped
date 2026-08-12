import { useCallback, useEffect, useId, useRef, type KeyboardEvent, type ReactNode } from 'react';

/**
 * The design system's dialog, as a component.
 *
 * The classes it draws with — `.clipped-scrim`, `.clipped-dialog` and its
 * title, body and actions — are the deck's own and already in
 * `components.css`. What is here is the behaviour a class cannot carry, and
 * which every dialog in the application has to have the same way (AGENTS.md
 * section 46): it is announced as a dialog, it is named by its own title,
 * Escape closes it, focus moves into it when it opens and back to whatever
 * opened it when it closes, and Tab cycles inside it rather than walking off
 * into the screen behind.
 *
 * # Why not the platform's `<dialog>`
 *
 * `HTMLDialogElement.showModal()` gives all of the above for nothing, and it is
 * the right answer the day it can be tested here: jsdom 27 — the environment
 * the whole interface's tests run in — does not implement `showModal`, so a
 * dialog built on it would be drawn in tests as an ordinary, non-modal,
 * always-visible element, and every assertion about modality would be an
 * assertion about nothing. A behaviour that cannot fail a test is not a
 * behaviour this repository claims (AGENTS.md section 54). The markup below is
 * what the deck draws anyway, so replacing the mechanism later changes this
 * file and nothing that consumes it.
 *
 * # What it does not do
 *
 * It does not close when the ground around it is clicked. This is a desktop
 * utility, and a stray click that discards what somebody was reading is worse
 * than one more press of Escape.
 */

/** What a dialog is given. */
export interface DialogProps {
  /**
   * The dialog's title, shown at the top and used as its accessible name — so
   * a screen reader announces the same words a sighted user reads, rather than
   * a second label written for it.
   */
  readonly title: string;
  /** Called when Escape is pressed, or the dialog closes itself. */
  readonly onClose: () => void;
  /** The dialog's content. */
  readonly children: ReactNode;
  /**
   * The row of controls at the bottom. Buttons, in the deck's own order:
   * the one that dismisses last, on the right.
   */
  readonly actions: ReactNode;
}

/** Everything inside `root` that can take focus, in tab order. */
function focusable(root: HTMLElement): readonly HTMLElement[] {
  const found = root.querySelectorAll<HTMLElement>(
    'a[href], button:not([disabled]), input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])',
  );
  return [...found];
}

/** A modal dialog. */
export function Dialog({ title, onClose, children, actions }: DialogProps): ReactNode {
  const titleId = useId();
  const panel = useRef<HTMLDivElement>(null);

  /*
   * Focus moves into the dialog when it opens and back to whatever opened it
   * when it closes. Without the second half, dismissing a dialog opened from a
   * button leaves the keyboard at the top of the document, which on a screen
   * with a timeline in it means finding your place again from the start.
   *
   * The panel itself takes focus rather than its first control: the first thing
   * a dialog has to do is be read, and a dialog that opens with a destructive
   * button focused is a dialog that can be confirmed by a Space bar pressed a
   * moment too late.
   */
  useEffect(() => {
    const opener = document.activeElement;
    panel.current?.focus();
    return () => {
      if (opener instanceof HTMLElement && opener.isConnected) {
        opener.focus();
      }
    };
  }, []);

  const onKeyDown = useCallback(
    (event: KeyboardEvent<HTMLDivElement>) => {
      if (event.key === 'Escape') {
        event.preventDefault();
        onClose();
        return;
      }
      if (event.key !== 'Tab') {
        return;
      }

      const inside = panel.current === null ? [] : focusable(panel.current);
      const first = inside[0];
      const last = inside[inside.length - 1];
      if (first === undefined || last === undefined) {
        return;
      }

      /*
       * The cycle is written out rather than left to the browser because the
       * elements behind the dialog are still in the tab order: this is a
       * `div` with `aria-modal`, not a top-layer dialog, so nothing else makes
       * the screen behind it inert. Focus starting on the panel itself — which
       * is where it is when the dialog opens — counts as being before the
       * first control, so Tab goes to it and Shift+Tab wraps to the last.
       */
      const at = document.activeElement;
      if (event.shiftKey && (at === first || at === panel.current)) {
        event.preventDefault();
        last.focus();
      } else if (!event.shiftKey && at === last) {
        event.preventDefault();
        first.focus();
      }
    },
    [onClose],
  );

  return (
    <div className="clipped-scrim">
      {/* eslint-disable-next-line jsx-a11y/no-noninteractive-element-interactions --
          the key handler is the dialog's own contract — Escape closes it and Tab
          cycles inside it — rather than an interaction bolted onto a static
          element, and `tabIndex` makes it a legitimate focus target. */}
      <div
        className="clipped-dialog"
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        tabIndex={-1}
        ref={panel}
        onKeyDown={onKeyDown}
      >
        <h2 className="clipped-dialog__title" id={titleId}>
          {title}
        </h2>
        {children}
        <div className="clipped-dialog__actions">{actions}</div>
      </div>
    </div>
  );
}
